//! The `restrict` contract, which is judgement J8 and is a promise about a block.
//!
//! Design: `spec/safe-memory/09-type-init-and-races.md` section 9.6, which is row Y8 of document 03,
//! and `spec/safe-memory/04-safety-model.md` section 4.6 for why it is not part of J1.
//!
//! Every other check in this crate is a question about one access: these bytes, this capability,
//! yes or no. C 6.7.3.1 is not that. It says that if an object reachable through a `restrict`
//! pointer declared in a block is modified anywhere in that block, then every access to that object
//! in that block is through that pointer, which is a statement about a pair of accesses and can only
//! be decided by comparing one against what the block has already done. So what goes in here is a
//! record with a lifetime rather than a check with an answer: a block that declares `restrict`
//! pointers opens a scope on entry, every access through one of those pointers tells the scope what
//! it reached, and the scope is what says whether two of them met.
//!
//! # What a scope is attached to
//!
//! A clique, which is the number `crates/rucc-lower/src/restrict.rs` hands out, one per block that
//! declares `restrict` pointers. Today that is always a parameter list, because that is the only
//! place the front end works one out, so a function has at most one clique and its block is the
//! whole body. Nothing here assumes that: the cliques are collected off the accesses and each gets
//! its own slot, so a clique for an inner block, which is tamnd/rucc#970, arrives without changing
//! this pass beyond where the two calls go.
//!
//! The scope is opened in the entry block and closed before every `return` and every tail call,
//! which is the body read as a block. A path that leaves by `longjmp` or by a call that does not
//! come back leaves the scope linked, and `runtime/rucc-safe-rt/src/restrict.rs` is where the magic
//! word that catches the stale link lives.
//!
//! # Why the accesses are what the cliques are read off
//!
//! The promise covers the block whether or not anything is accessed through the pointers, but a
//! scope no access ever asks about answers nothing, and opening one would be two calls and a hundred
//! and twelve bytes of stack for a function that had a `restrict` parameter and never dereferenced
//! it. So a clique with no access in it gets no scope, which loses nothing and is the difference
//! between `memcpy` paying for this and every function that passes a `restrict` pointer on paying
//! for it.
//!
//! How many pointers the block declares travels the same way, as the largest base any access of the
//! clique carries. That is a lower bound on what was declared rather than the number itself, and the
//! runtime only uses it to say how many of the four entries mean anything, so an under count costs
//! nothing: an access through base three finds entry three whatever the scope was told.

use std::collections::BTreeMap;

use rucc_ir::{
    Block, Extra, Func, Inst, InstData, MemInfo, MemOrder, Opcode, Restrict, Type, Value,
};

/// How many bytes one scope takes on the stack.
///
/// `rucc_safe_rt::restrict::Scope` is a magic word, a clique, a base count, four twenty four byte
/// entries and a link, which is one hundred and twelve bytes on a sixty four bit target. On a thirty
/// two bit one the link is four bytes and the structure is one hundred and eight, rounded up to one
/// hundred and twelve because the entries hold addresses as `u64` and give the whole thing an
/// alignment of eight. So one constant is right everywhere, and the runtime's own test of the
/// offsets is what keeps it right.
pub const SCOPE: u64 = 112;

/// What a scope has to be aligned to, for the reason the size is what it is.
pub const ALIGN: u32 = 8;

/// What one function's `restrict` pointers cost it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Kept {
    /// Accesses that were given a check against the scope they are in.
    pub promised: usize,
    /// Scopes opened, which is one per clique that has an access in it.
    pub scoped: usize,
}

/// Opens a scope for every `restrict` block of a function and checks every access inside one.
///
/// Runs after the rest of the insertion pass, so the check lands between the bounds check and the
/// access: an access that is out of its object is refused for that before it is asked about who else
/// reached the byte, which is the order the two answers are worth having in.
///
/// `width` is the target's pointer width in bytes, for the same reason [`crate::insert`] takes one.
pub fn promise(func: &mut Func, width: u64) -> Kept {
    let mut kept = Kept::default();
    let mut accesses: Vec<Inst> = Vec::new();
    let mut bases: BTreeMap<u16, u16> = BTreeMap::new();
    let blocks: Vec<Block> = func.blocks().collect();
    for block in blocks {
        let insts: Vec<Inst> = func.insts(block).collect();
        for inst in insts {
            let Some(named) = through(func, inst) else { continue };
            accesses.push(inst);
            let seen = bases.entry(named.clique).or_default();
            *seen = (*seen).max(named.base);
        }
    }
    if accesses.is_empty() {
        return kept;
    }
    let Some(entry) = func.entry() else { return kept };

    // The scopes first, so that an access has something to be checked against however early in the
    // entry block it is. Which order the cliques are opened in does not matter, since the runtime
    // finds one by its number rather than by its depth, and the map gives a stable one anyway.
    let mut slots: BTreeMap<u16, Value> = BTreeMap::new();
    for (&clique, &count) in &bases {
        let Some(slot) = opened(func, entry, clique, count) else { continue };
        slots.insert(clique, slot);
        kept.scoped += 1;
    }
    closed(func, &slots);

    for access in accesses {
        let Some(named) = through(func, access) else { continue };
        if !slots.contains_key(&named.clique) {
            continue;
        }
        if promised(func, access, width) {
            kept.promised += 1;
        }
    }
    kept
}

/// Which `restrict` pointer an access went through, for the accesses that went through one.
///
/// A base of zero with a clique that is not is an access inside a `restrict` block that the front
/// end could not trace back to a declaration, which is most of the accesses in most of those blocks.
/// It is not evidence of anything, so it asks nothing.
fn through(func: &Func, inst: Inst) -> Option<Restrict> {
    if !matches!(func[inst].opcode, Opcode::Load | Opcode::Store) {
        return None;
    }
    let Extra::Mem(at) = func[inst].extra else { return None };
    let named = func[at].restrict;
    (named.clique != 0 && named.base != 0).then_some(named)
}

/// Reserves a scope in the entry block and opens it, giving back its address.
///
/// The slot is an `alloca` at the top of the entry block, where the verifier wants every one of
/// them and where one in a loop would otherwise grow the stack every time round. The `restrict_enter`
/// goes directly after it rather than at the top too, so that a second clique's slot landing in
/// front of this one cannot land between this one and the call that publishes it.
fn opened(func: &mut Func, entry: Block, clique: u16, count: u16) -> Option<Value> {
    let first = func.insts(entry).next()?;
    let span = func.span(first);
    let empty = MemInfo {
        size: SCOPE,
        align: ALIGN,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    };

    let mem = func.add_mem(empty);
    let data = InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) };
    let slot = func.create_inst(data, &[Type::PTR], span);
    func.insert_before(slot, first);
    let address = func[slot].results().next()?;

    // The base field carries how many pointers the block declares rather than which one this is,
    // which is what the opcode's documentation says and what the runtime reads it as.
    let told = MemInfo { restrict: Restrict { clique, base: count }, ..empty };
    let mem = func.add_mem(told);
    let args = func.push_values(&[address]);
    let data = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::RestrictEnter) };
    let enter = func.create_inst(data, &[], span);
    func.insert_after(enter, slot);
    Some(address)
}

/// Closes every scope on every path that leaves the function.
///
/// A `return` and a tail call, which are the two terminators that give the frame back. An
/// `unreachable` is not one: nothing after it runs, so a call there would never happen, and the
/// paths that really do leave without returning are the ones the runtime's magic word is for.
fn closed(func: &mut Func, slots: &BTreeMap<u16, Value>) {
    let blocks: Vec<Block> = func.blocks().collect();
    for block in blocks {
        let Some(end) = func.terminator(block) else { continue };
        if !matches!(func[end].opcode, Opcode::Return | Opcode::TailCall) {
            continue;
        }
        let span = func.span(end);
        // Innermost first, which is the only order that puts the enclosing scope back rather than
        // one that is already gone. The scopes were opened in order, so the last clique is the
        // innermost, and each of these lands after the one put in before it.
        for &slot in slots.values().rev() {
            let args = func.push_values(&[slot]);
            let data = InstData { args, ..InstData::new(Opcode::RestrictLeave) };
            let leave = func.create_inst(data, &[], span);
            func.insert_before(leave, end);
        }
    }
}

/// Puts a `check_restrict_read` or a `check_restrict_write` immediately before one access.
///
/// Two opcodes rather than one with a flag on it, for the reason `rucc_ir::Opcode` gives. Which of
/// the two it is is what decides whether a pair of accesses is a violation at all: two reads through
/// two `restrict` pointers of one block are nothing, because the contract is about an object being
/// modified.
fn promised(func: &mut Func, access: Inst, width: u64) -> bool {
    let Some(pointer) = crate::pointer_of(func, access) else { return false };
    let Extra::Mem(at) = func[access].extra else { return false };
    let mut info = func[at];
    info.size = crate::covered(func, access, info.size, width);
    // An access whose width nothing states reaches no bytes anybody can name, and a range of
    // nothing is a range no other pointer can have overlapped.
    if info.size == 0 {
        return false;
    }
    // Not the padding after the member, for the reason a bounds check does not carry it: what this
    // is about is the bytes the access touches.
    info.owns = 0;
    // The question is about addresses rather than about types, so carrying the node the access named
    // would suggest the check compares against it.
    info.tbaa = None;

    let write = func[access].opcode == Opcode::Store;
    let opcode = if write { Opcode::CheckRestrictWrite } else { Opcode::CheckRestrictRead };
    let span = func.span(access);
    let args = func.push_values(&[pointer]);
    let extra = Extra::Mem(func.add_mem(info));
    let data = InstData { args, extra, ..InstData::new(opcode) };
    let check = func.create_inst(data, &[], span);
    func.insert_before(check, access);
    true
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, Flags, IntPred, Module, Signature, print_func, verify_func};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::*;

    /// A module for the printer and the verifier to resolve names against.
    fn module(names: &mut Interner, unit: &str) -> Module {
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        Module::new(names.intern(unit), &target)
    }

    /// The payload of a four byte access through one `restrict` pointer.
    fn through(named: Restrict) -> MemInfo {
        MemInfo {
            size: 0,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: named,
        }
    }

    /// A function that reads through one of its parameters and writes through the other.
    ///
    /// Which is `combine(int *restrict to, int *restrict from)`, the shape the contract was put in
    /// the language for and the one `tests/safety/two-restrict-pointers-that-alias.c` is about.
    fn combine(names: &mut Interner, to: Restrict, from: Restrict) -> Func {
        let word = Type::int(32);
        let mut func = Func::new(
            names.intern("combine"),
            Signature::new().with_params(&[Type::PTR, Type::PTR]),
        );
        let entry = func.create_block();
        let out = func.append_param(entry, Type::PTR);
        let inp = func.append_param(entry, Type::PTR);
        let mut b = Builder::new(&mut func, entry);
        let value = b.load(word, inp, through(from), Flags::default());
        b.store(value, out, through(to), Flags::default());
        b.ret(&[]);
        func
    }

    #[test]
    fn a_block_that_declares_restrict_pointers_opens_a_scope_and_closes_it() {
        // The whole pass in one function. The slot is at the top of the entry block where every
        // other `alloca` goes, the scope is published before anything can ask about it, each access
        // says which pointer it went through, and the scope is taken away before the block ends.
        let mut names = Interner::new();
        let unit = module(&mut names, "combine.c");
        let mut func =
            combine(&mut names, Restrict { clique: 1, base: 1 }, Restrict { clique: 1, base: 2 });

        assert_eq!(promise(&mut func, 8), Kept { promised: 2, scoped: 1 });

        assert_eq!(
            print_func(&unit, &func, &names),
            "func @combine(ptr, ptr), linkage(external) {\n\
             block0(%0: ptr, %1: ptr):\n    \
             %2 = alloca, size 112, align 8\n    \
             restrict_enter %2, size 112, align 8, restrict(1, 2)\n    \
             check_restrict_read %1, size 4, align 4, restrict(1, 2)\n    \
             %3 = load.i32 %1, align 4, restrict(1, 2)\n    \
             check_restrict_write %0, size 4, align 4, restrict(1, 1)\n    \
             store %3 -> %0, align 4, restrict(1, 1)\n    \
             restrict_leave %2\n    \
             return\n\
             }\n"
        );

        if let Err(errors) = verify_func(&unit, &func, &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn a_function_with_no_restrict_pointers_pays_nothing() {
        // Which is nearly every function in nearly every program. A pass that opened a scope for
        // one of those would put two calls and a hundred and twelve bytes of stack in front of code
        // that promised nothing, and there would be nothing for them to answer.
        let mut names = Interner::new();
        let unit = module(&mut names, "plain.c");
        let mut func = combine(&mut names, Restrict::NONE, Restrict::NONE);
        let before = print_func(&unit, &func, &names);

        assert_eq!(promise(&mut func, 8), Kept::default());
        assert_eq!(print_func(&unit, &func, &names), before);
    }

    #[test]
    fn an_access_the_names_said_nothing_about_asks_nothing() {
        // A base of zero inside a clique that is not is an access the front end could not trace
        // back to a declaration, which is most of the accesses in most `restrict` blocks. It is not
        // evidence of anything, so it gets no check, and the block still opens a scope because the
        // other access in it does ask.
        let mut names = Interner::new();
        let unit = module(&mut names, "untraced.c");
        let mut func =
            combine(&mut names, Restrict { clique: 1, base: 0 }, Restrict { clique: 1, base: 2 });

        assert_eq!(promise(&mut func, 8), Kept { promised: 1, scoped: 1 });

        let printed = print_func(&unit, &func, &names);
        assert!(printed.contains("check_restrict_read"), "{printed}");
        assert!(!printed.contains("check_restrict_write"), "{printed}");
    }

    #[test]
    fn a_block_nothing_is_accessed_through_opens_no_scope() {
        // A function that takes a `restrict` pointer and passes it on without dereferencing it. The
        // promise is still made, and a scope nothing ever asks about answers nothing, so the two
        // calls would be cost with no check behind them.
        let mut names = Interner::new();
        let unit = module(&mut names, "passed-on.c");
        let mut func =
            combine(&mut names, Restrict { clique: 4, base: 0 }, Restrict { clique: 4, base: 0 });

        assert_eq!(promise(&mut func, 8), Kept::default());

        let printed = print_func(&unit, &func, &names);
        assert!(!printed.contains("restrict_enter"), "{printed}");
    }

    #[test]
    fn how_many_pointers_the_block_declares_is_what_the_scope_is_told() {
        // The runtime uses it to say how many of its four entries mean anything. What travels is
        // the largest base any access carries, which is a lower bound on what was declared and is
        // all this can see, and an access through base three finds entry three either way.
        let mut names = Interner::new();
        let unit = module(&mut names, "three.c");
        let mut func =
            combine(&mut names, Restrict { clique: 7, base: 3 }, Restrict { clique: 7, base: 1 });

        assert_eq!(promise(&mut func, 8), Kept { promised: 2, scoped: 1 });

        let printed = print_func(&unit, &func, &names);
        assert!(
            printed.contains("restrict_enter %2, size 112, align 8, restrict(7, 3)"),
            "{printed}"
        );
    }

    #[test]
    fn the_scope_is_closed_on_every_path_that_leaves() {
        // A block with two exits. Leaving the scope linked on one of them would leave the per
        // thread list pointing into a frame that has gone, and the next access of that clique would
        // be judged against whatever is in that storage now.
        let mut names = Interner::new();
        let unit = module(&mut names, "two-ways.c");
        let word = Type::int(32);
        let mut func =
            Func::new(names.intern("pick"), Signature::new().with_params(&[Type::PTR, Type::PTR]));
        let entry = func.create_block();
        let out = func.append_param(entry, Type::PTR);
        let inp = func.append_param(entry, Type::PTR);
        let yes = func.create_block();
        let no = func.create_block();

        let mut b = Builder::new(&mut func, entry);
        let value = b.load(word, inp, through(Restrict { clique: 1, base: 2 }), Flags::default());
        let zero = b.iconst(word, 0);
        let taken = b.icmp(IntPred::Ne, value, zero);
        b.br_if(taken, yes, &[], no, &[]);
        let mut b = Builder::new(&mut func, yes);
        b.store(value, out, through(Restrict { clique: 1, base: 1 }), Flags::default());
        b.ret(&[]);
        let mut b = Builder::new(&mut func, no);
        b.ret(&[]);

        assert_eq!(promise(&mut func, 8), Kept { promised: 2, scoped: 1 });

        let printed = print_func(&unit, &func, &names);
        assert_eq!(printed.matches("restrict_enter").count(), 1, "{printed}");
        assert_eq!(printed.matches("restrict_leave").count(), 2, "{printed}");
        if let Err(errors) = verify_func(&unit, &func, &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn two_blocks_of_one_function_get_a_scope_each() {
        // Two cliques in one function, which is what an inner block declaring `restrict` pointers
        // will be. Nothing produces one today, and the pass is written off the cliques rather than
        // off the parameter list so that the day one arrives this is already right.
        let mut names = Interner::new();
        let unit = module(&mut names, "two-cliques.c");
        let mut func =
            combine(&mut names, Restrict { clique: 2, base: 1 }, Restrict { clique: 5, base: 1 });

        assert_eq!(promise(&mut func, 8), Kept { promised: 2, scoped: 2 });

        let printed = print_func(&unit, &func, &names);
        assert!(printed.contains("restrict(2, 1)"), "{printed}");
        assert!(printed.contains("restrict(5, 1)"), "{printed}");
        // Innermost first, so that leaving one puts the other back rather than a scope that has
        // already gone.
        let leaves: Vec<&str> = printed.lines().filter(|line| line.contains("restrict_")).collect();
        assert_eq!(leaves.len(), 6, "{printed}");
    }
}
