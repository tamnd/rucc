//! Which functions never come back, worked out from their bodies, and the end of the block after
//! every call to one.
//!
//! gcc's `ipa-pure-const` does this from `-O1` up. A function whose body has no way to reach a
//! `return` is marked `noreturn` the same as if it had been declared so, and a call to it ends the
//! block it is in. Linux 6.1's `skb_panic` in net/core/skbuff.c is `static`, prints and ends in
//! `BUG()`, so gcc writes nothing after a call to it. objtool works the same thing out from the
//! object and reports whatever comes after the call as an instruction nothing reaches, which was
//! the `jmp` back into `skb_put` that rucc wrote there.
//!
//! The answer goes where the front end puts the one for a declared `noreturn`: an
//! `unreachable_hint` right after the call, which `crate::simplify_cfg` turns into the end of the
//! block. The function gets [`AttrSet::NORETURN`] as well, so that a pass reading attributes, the
//! branch predictor for one, sees what it would have seen had the source said so.
//!
//! # What it takes
//!
//! A body is walked from its entry, and a block stops at a stop: an `unreachable`, a `trap`, a hint,
//! or a call to a function that is `noreturn` by declaration or by this analysis. A function is
//! `noreturn` when the walk reaches no `return` and no tail call to something that comes back. A
//! loop with no way out is a way of not coming back too, and gcc counts it the same.
//!
//! Only a definition the link cannot replace is read, which is [`CallGraph::trusted_body`], and a
//! `naked` one is never read, since its body is a template whose `ret` the walk cannot see.
//!
//! # Which way the fixed point goes
//!
//! Every function starts as coming back and is moved when its body says otherwise. A pair of
//! functions that only call each other then stays coming back, which is the answer that costs a
//! missed fold rather than code that falls off the end of a call that did return.

use rucc_ir::{AttrSet, Extra, Func, Inst, InstData, Module, Opcode, Pic};

use crate::callgraph::CallGraph;

/// What `-fipa-pure-const` and its `-fno-` form toggle, which is where gcc keeps this. Not the name
/// of a pass.
pub const NAME: &str = "ipa-pure-const";

/// Works out which functions never come back, marks them, and ends the block after every call to
/// one. Says how many calls it ended a block after.
pub fn annotate(module: &mut Module, pic: Pic) -> usize {
    let graph = CallGraph::of(module, pic);
    let declared = |module: &Module, name| {
        graph
            .node(name)
            .and_then(|node| graph.func(node))
            .is_some_and(|id| module[id].attrs.set.contains(AttrSet::NORETURN))
    };
    let answers = graph.solve(
        |_| false,
        |node, answers| {
            let Some(id) = graph.trusted_body(node) else { return false };
            let func = &module[id];
            if func.attrs.set.contains(AttrSet::NAKED) {
                return false;
            }
            let gone = |inst| {
                callee(func, inst).is_some_and(|name| {
                    declared(module, name)
                        || graph.node(name).is_some_and(|node| answers[node.index()])
                })
            };
            !comes_back(func, gone)
        },
    );
    let mut ended = 0;
    for node in graph.nodes() {
        if !answers[node.index()] {
            continue;
        }
        if let Some(id) = graph.func(node) {
            module[id].attrs.set |= AttrSet::NORETURN;
        }
    }
    let ids: Vec<_> = module.funcs().collect();
    for id in ids {
        let func = &mut module[id];
        if func.is_declaration() {
            continue;
        }
        let insts: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
        for inst in insts {
            if func[inst].opcode != Opcode::Call {
                continue;
            }
            let Some(name) = callee(func, inst) else { continue };
            if !graph.node(name).is_some_and(|node| answers[node.index()]) {
                continue;
            }
            let next = func.next_inst(inst);
            if next.is_some_and(|next| stops(func, next)) {
                continue;
            }
            let span = func.span(inst);
            let hint = func.create_inst(InstData::new(Opcode::UnreachableHint), &[], span);
            func.insert_after(hint, inst);
            ended += 1;
        }
    }
    ended
}

/// The name a call goes to, when it goes to one.
fn callee(func: &Func, inst: Inst) -> Option<rucc_base::Symbol> {
    if !matches!(func[inst].opcode, Opcode::Call | Opcode::TailCall) {
        return None;
    }
    match func[inst].extra {
        Extra::Call(at) => func[at].callee,
        _ => None,
    }
}

/// Whether control cannot get past an instruction whatever is called.
fn stops(func: &Func, inst: Inst) -> bool {
    matches!(func[inst].opcode, Opcode::Unreachable | Opcode::UnreachableHint | Opcode::Trap)
}

/// Whether a `return` can be reached from the entry, with `gone` saying which calls do not come
/// back.
fn comes_back(func: &Func, gone: impl Fn(Inst) -> bool) -> bool {
    let Some(entry) = func.entry() else { return true };
    let mut seen = vec![false; func.blocks().map(|b| b.index() + 1).max().unwrap_or(0)];
    let mut work = vec![entry];
    seen[entry.index()] = true;
    while let Some(block) = work.pop() {
        let mut through = true;
        for inst in func.insts(block) {
            if stops(func, inst) || gone(inst) {
                through = false;
                break;
            }
            if matches!(func[inst].opcode, Opcode::Return | Opcode::TailCall) {
                return true;
            }
        }
        if !through {
            continue;
        }
        let Some(term) = func.terminator(block) else { continue };
        for to in func.successors(term) {
            if !seen[to.block.index()] {
                seen[to.block.index()] = true;
                work.push(to.block);
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use rucc_base::Interner;

    const HEAD: &str = "; ModuleID = 't.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"

";

    /// The module, after the analysis, printed.
    fn after(body: &str) -> (String, usize) {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let ended = annotate(&mut module, Pic::Executable);
        (rucc_ir::print(&module, &names), ended)
    }

    #[test]
    fn a_call_to_a_static_function_that_ends_in_a_bug_ends_the_block() {
        // `skb_panic` and `skb_put` in 6.1's net/core/skbuff.c, with the warning and the bug
        // entry left out.
        let (printed, ended) = after(
            "func @warn(), linkage(external);

func @panic(i32), linkage(internal), attrs(noinline) {
block0(%0: i32):
    call @warn() : ()
    inline_asm.volatile.nomem \"ud2\", \"\", \"\"()
    unreachable
}

func @put(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 3
    %2 = icmp sgt %0, %1
    br_if %2, block1, block2

block1:
    call @panic(%0) : (i32)
    jump block2

block2:
    return %0
}
",
        );
        assert_eq!(ended, 1, "{printed}");
        assert!(printed.contains("call @panic(%0) : (i32)\n    unreachable_hint\n"), "{printed}");
        assert!(
            printed.contains("func @panic(i32), linkage(internal), attrs(noreturn, noinline)"),
            "{printed}"
        );
    }

    #[test]
    fn a_function_that_can_return_or_could_be_replaced_is_left_alone() {
        let (printed, ended) = after(
            "func @warn(), linkage(external);

func @sometimes(i32), linkage(internal) {
block0(%0: i32):
    %1 = iconst.i32 0
    %2 = icmp ne %0, %1
    br_if %2, block1, block2

block1:
    unreachable

block2:
    return
}

func @weakly(), linkage(weak) {
block0:
    unreachable
}

func @f(i32), linkage(external) {
block0(%0: i32):
    call @sometimes(%0) : (i32)
    call @weakly() : ()
    call @warn() : ()
    return
}
",
        );
        assert_eq!(ended, 0, "{printed}");
        assert!(!printed.contains("noreturn"), "{printed}");
    }

    #[test]
    fn a_function_whose_every_way_out_is_a_call_that_does_not_come_back_does_not_either() {
        // Down a chain, and round a loop with no way out, which gcc counts as not coming back.
        let (printed, ended) = after(
            "func @abort(), linkage(external), attrs(noreturn);

func @spin(), linkage(internal) {
block0:
    jump block1

block1:
    jump block1
}

func @die(i32), linkage(internal) {
block0(%0: i32):
    %1 = iconst.i32 0
    %2 = icmp ne %0, %1
    br_if %2, block1, block2

block1:
    call @abort() : ()
    jump block3

block2:
    call @spin() : ()
    jump block3

block3:
    return
}

func @f(i32), linkage(external) {
block0(%0: i32):
    call @die(%0) : (i32)
    return
}
",
        );
        assert!(printed.contains("call @die(%0) : (i32)\n    unreachable_hint\n"), "{printed}");
        assert!(printed.contains("call @spin() : ()\n    unreachable_hint\n"), "{printed}");
        assert_eq!(ended, 2, "{printed}");
    }
}
