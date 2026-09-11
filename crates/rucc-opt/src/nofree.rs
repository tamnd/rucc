//! Which functions cannot free memory, and writing that onto the calls to them.
//!
//! Design: `spec/safe-memory/07-check-elimination.md` section 7.5, which asks for a summary per
//! function recording "which pointer parameters are dereferenced and over what range, which are
//! freed, which escape, and whether the function can free memory at all", and then says which of
//! the four matters most: "The last is the one that unlocks temporal elimination, a call to a
//! function summarized as `nofree` does not kill liveness facts, and `nofree` is true of a very
//! large fraction of leaf functions." Document 08 section 8.8 puts a number on it, and the number
//! is why this is built before the other three: without it a fact dies at every call, and the
//! temporal checks cost forty per cent rather than five.
//!
//! This is that one field. The dereferenced ranges, the freed parameters and the escaping ones are
//! the rest of the box on milestone S4 and are not here.
//!
//! # Where the answer goes
//!
//! Whether the function being called can free is a fact about a different function, and a pass is
//! given one function and not the module around it. So the answer is not kept in a table a pass
//! would have to be handed. It is written into the IR, as [`Flags::NOFREE`] on the call site, by
//! [`annotate`] before the pipeline starts, and `crate::discharge` reads it off the instruction in
//! front of it. That is what the frontend already does with a call that never comes back: it puts
//! an `unreachable` after the call rather than expecting every later pass to look the callee up.
//!
//! Two deviations from where the design puts this, both deliberate and both worth writing down.
//! Section 7.5 and document 15's crate table say `rucc-lto` records the summaries, and `rucc-lto`
//! is a crate that holds its layer rank and nothing else until M8. This is here instead, one
//! translation unit at a time, which is the part of the answer a compile of one file can have and
//! is what the pass consuming it can use today. The second is that the summary is not a summary
//! anybody can read back: it is spent on the call sites and thrown away. The day link time
//! optimization arrives it will want a record that survives the file it was worked out in, and
//! [`Summaries`] is the shape of it.
//!
//! # What it takes to be nofree
//!
//! A function this module defines is nofree when every call in its body goes somewhere nofree and
//! it ends no lifetime itself. Everything else it can do is arithmetic, memory traffic and control
//! flow, none of which ends anything.
//!
//! A function this module does not define is nofree only if the `NEVER_FREES` table names it. That
//! is the same bar `crate::purity` sets for its library table and for the same reason: a name
//! missing from the table costs a check that stays, and a wrong name in the table costs a check
//! that goes when it should not have, which is a hole in the safety this compiler is for. Nothing
//! goes in there unless the standard says what the function does and what it does is not freeing.
//!
//! The one exception to needing a standard is this compiler's own runtime, which has a table of
//! its own. Those bodies are in this repository, so the reason for the bar does not apply to them
//! and reading what they do is the check the standard stands in for everywhere else.
//!
//! Not defining it and not declaring it are two different things, and the table is asked in both
//! cases. A module usually has a declaration for every name it calls, because that is what the C
//! it was compiled from had to have, but `rucc-safety` puts calls in that no source wrote: a
//! witness at every boundary crossing, and a redirect of a library call to the wrapper around it.
//! Both intern a name and emit a call to it without adding a function to hang the name on. Asking
//! only the declarations meant the table was never asked about either of them, so the wrapper
//! spelling `never_frees` handles below did nothing for the calls it was written for, and on the
//! SQLite amalgamation 1322 call sites read as possible frees on that account alone. See
//! tamnd/rucc#810.
//!
//! `meta_end` and `meta_transfer` are counted as freeing wherever they appear. Nothing emits
//! either of them yet, so today this costs nothing, and when the instrumentation starts ending
//! lifetimes it will be conservative rather than wrong. The refinement is that `meta_end` on an
//! automatic instance the callee created in its own frame cannot be about storage the caller had a
//! pointer to before the call, but knowing that needs the storage class the matching `meta_begin`
//! carries and the escape analysis in section 7.6, so it waits for them.
//!
//! A call through an address, inline assembly and a target intrinsic are all counted as freeing.
//! The first two could reach anything. The third could not, since a target intrinsic is a machine
//! instruction, but the intrinsic set is open and named rather than enumerated, so nothing here
//! knows which one it is looking at, and [`crate::purity`] answers the same way for the same
//! reason.
//!
//! # Recursion, and which way the fixed point goes
//!
//! Every defined function starts in the set and is taken out when something it calls is not in it,
//! until nothing changes. That is the least fixed point of "can free", and starting the other way
//! round would be wrong in the direction that matters less but is still wrong: a function that
//! calls itself and frees nothing would never get into the set, and a pair of functions that call
//! each other and free nothing would keep each other out of it forever.
//!
//! # What is trusted about a definition
//!
//! A `weak` or `common` definition is not trusted, because the linker is allowed to throw it away
//! and take a definition from another object instead, and this analysis read the one that will not
//! run.
//!
//! An ordinary external definition is trusted when the link that is coming puts every name in the
//! same program, and not otherwise. A shared library's exported symbol can be interposed at run
//! time, by `LD_PRELOAD` or by an earlier object in the search order, and the definition that runs
//! is then one this module never saw, so under `-fPIC` a name like that is left out of the set and
//! every caller of it pays for the possibility. This used to be an assumption instead, written down
//! in this comment and true of nothing but an executable, which is what tamnd/rucc#756 turned into
//! a question the compiler can actually ask.
//!
//! `-fno-semantic-interposition` puts the trust back, and that is a promise the build makes rather
//! than anything deduced here. Every distribution makes it, because a library that cannot believe
//! its own bodies pays for an interposition that almost never happens. `-fvisibility=hidden` gets
//! the same result by making the names uninterposable, which is a stronger thing to say and needs
//! no promise.

use std::collections::HashSet;

use rucc_base::{Interner, Symbol};
use rucc_ir::{Extra, Flags, Func, FuncId, Inst, Linkage, Module, Opcode, Pic};

/// What is known about which functions cannot free.
///
/// Built from the module once, because the answer belongs to the callee and there is one callee
/// and many call sites, which is the same shape [`crate::purity::Facts`] has.
#[derive(Debug, Clone, Default)]
pub struct Summaries {
    nofree: HashSet<Symbol>,
}

impl Summaries {
    /// Nothing known about anything, which answers no to every question and is correct.
    #[must_use]
    pub fn nothing() -> Self {
        Self::default()
    }

    /// Works out which of the module's functions cannot free.
    ///
    /// The interner is here for the library table, which is written in text because that is what
    /// the C standard names the functions. Nothing after this call needs it.
    #[must_use]
    pub fn of_module(module: &Module, names: &Interner, pic: Pic) -> Self {
        let ids: Vec<FuncId> = module.funcs().collect();
        let mut nofree = HashSet::new();
        for &id in &ids {
            let func = &module[id];
            if func.is_declaration() {
                if never_frees(names.resolve(func.name)) {
                    nofree.insert(func.name);
                }
            } else if trusted(func, pic) {
                // Optimistic, and narrowed below. See the module comment for which way round the
                // fixed point has to go and what starting from the other end would cost.
                nofree.insert(func.name);
            }
        }
        // The same question again, for a name the module calls and has no function of any kind
        // for. See the module comment for why those exist and why the loop above cannot see them.
        let present: HashSet<Symbol> = ids.iter().map(|&id| module[id].name).collect();
        for &id in &ids {
            let func = &module[id];
            for block in func.blocks() {
                for inst in func.insts(block) {
                    let Some(callee) = called(func, inst) else { continue };
                    if present.contains(&callee) || nofree.contains(&callee) {
                        continue;
                    }
                    if never_frees(names.resolve(callee)) {
                        nofree.insert(callee);
                    }
                }
            }
        }
        loop {
            let mut settled = true;
            for &id in &ids {
                let func = &module[id];
                if func.is_declaration() || !nofree.contains(&func.name) {
                    continue;
                }
                if frees(func, &nofree) {
                    nofree.remove(&func.name);
                    settled = false;
                }
            }
            if settled {
                return Self { nofree };
            }
        }
    }

    /// Whether a call to that name reaches nothing that ends a lifetime.
    #[must_use]
    pub fn cannot_free(&self, name: Symbol) -> bool {
        self.nofree.contains(&name)
    }

    /// How many names are in the set, which is what a caller reporting the summary wants.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nofree.len()
    }

    /// Whether nothing at all was established.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nofree.is_empty()
    }
}

/// Works out the summaries and marks every call site they vouch for, saying how many it marked.
///
/// Only sets the flag, never clears one. The flag is an assertion like the rest of them, so a
/// caller that put one there meant it, and this adds the ones it can prove rather than replacing
/// what it finds.
pub fn annotate(module: &mut Module, names: &Interner, pic: Pic) -> usize {
    let summaries = Summaries::of_module(module, names, pic);
    let mut marked = 0;
    let ids: Vec<FuncId> = module.funcs().collect();
    for id in ids {
        if module[id].is_declaration() {
            continue;
        }
        let func = &mut module[id];
        let insts: Vec<Inst> =
            func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
        for inst in insts {
            // A tail call as well as a call. Nothing after a tail call needs the fact, but the
            // flag says what the call reaches rather than what happens after it, and a call that
            // carried it under one spelling and not the other would read as a disagreement.
            if !matches!(func[inst].opcode, Opcode::Call | Opcode::TailCall) {
                continue;
            }
            let Extra::Call(at) = func[inst].extra else { continue };
            let Some(callee) = func[at].callee else { continue };
            if !summaries.cannot_free(callee) || func[inst].flags.contains(Flags::NOFREE) {
                continue;
            }
            func[inst].flags |= Flags::NOFREE;
            marked += 1;
        }
    }
    marked
}

/// The name a call names, or `None` when the instruction is not a call to a name.
fn called(func: &Func, inst: Inst) -> Option<Symbol> {
    if !matches!(func[inst].opcode, Opcode::Call | Opcode::TailCall) {
        return None;
    }
    let Extra::Call(at) = func[inst].extra else { return None };
    func[at].callee
}

/// Whether the definition in hand is the one that will run.
///
/// Two linkages say otherwise whatever the link is. `weak` and `common` are both definitions the
/// linker is allowed to throw away in favour of one from another object, so a body with either of
/// them is one this analysis may have read for nothing.
///
/// The third way is the link itself. Under `-fPIC` an exported name is one the dynamic linker may
/// find another definition of first, so this body is not the one that runs however plainly it is
/// written here, and `pic` is what carries that. A `static` is never such a name and neither is one
/// marked hidden or protected, which is the same rule the code generator uses to decide which
/// addresses go through the table and is why it is the same method.
fn trusted(func: &Func, pic: Pic) -> bool {
    !matches!(func.linkage, Linkage::Weak | Linkage::Common)
        && !pic.replaceable(func.linkage, func.visibility)
}

/// Whether anything in this body could end the lifetime of any storage.
///
/// `nofree` is the set as it stands, so this is asked again each time the set shrinks.
fn frees(func: &Func, nofree: &HashSet<Symbol>) -> bool {
    for block in func.blocks() {
        for inst in func.insts(block) {
            match func[inst].opcode {
                Opcode::MetaEnd | Opcode::MetaTransfer => return true,
                Opcode::Call | Opcode::TailCall => {
                    let Extra::Call(at) = func[inst].extra else { return true };
                    let Some(callee) = func[at].callee else { return true };
                    if !nofree.contains(&callee) {
                        return true;
                    }
                }
                Opcode::CallIndirect | Opcode::InlineAsm | Opcode::TargetIntrinsic => return true,
                _ => {}
            }
        }
    }
    false
}

/// The functions outside this module that are known to end no lifetime.
///
/// Short on purpose, and the entries are the ones whose behaviour the C standard writes down. The
/// allocating ones are here because handing out new storage is not ending old storage, and
/// `realloc` is deliberately absent: it may free what it was given.
///
/// Sorted, and a test checks that it is sorted and says each name once.
const NEVER_FREES: &[&str] = &[
    "abs",
    "aligned_alloc",
    "bcopy",
    "bzero",
    "calloc",
    "imaxabs",
    "labs",
    "llabs",
    "malloc",
    "memchr",
    "memcmp",
    "memcpy",
    "memmove",
    "memset",
    "posix_memalign",
    "pread",
    "pwrite",
    "read",
    "readv",
    "recv",
    "send",
    "stpcpy",
    "strcat",
    "strchr",
    "strcmp",
    "strcpy",
    "strcspn",
    "strlen",
    "strncat",
    "strncmp",
    "strncpy",
    "strnlen",
    "strpbrk",
    "strrchr",
    "strspn",
    "strstr",
    "write",
    "writev",
];

/// What `rucc-safety` puts in front of the name of a function it interposes.
///
/// The same string as `rucc_safety::wrap::PREFIX`, written again here because `rucc-opt` and
/// `rucc-safety` are the same layer rank and neither can see the other. Repeating it is safe in
/// the direction that matters: if the two ever disagree, a wrapped call stops being recognised and
/// a check stays, which costs nothing but the check.
const WRAPPER_PREFIX: &str = "__rucc_wrap_";

/// The entry points of this compiler's own runtime that end no lifetime.
///
/// A different table from the one above because the bar is different, not because the answer is.
/// The C table takes a name only where the standard says what the function does, since nothing in
/// a build can see the body. These bodies are in this repository, in `runtime/rucc-safe-rt`, and
/// what each of them does is read the lifetime plane and then either return a number or report.
/// Reporting ends the program under `-fsafety=detect` and records under `-fsafety=recover`, and
/// neither of those hands storage back. The last two write a plane rather than reading one, which
/// is memory of the runtime's own and not storage the program was ever given.
///
/// These eight and no more. The rest of what the runtime exports is the allocator's own bookkeeping,
/// `__rucc_alloc_purge` and the frame calls and the rest, and those are exactly the things that do
/// end a lifetime. Nothing generated calls them, so leaving them out costs nothing, and a name
/// added here without reading what it does would be a hole in the safety this compiler is for.
///
/// Sorted, and the same test that checks the table above checks this one.
const RUNTIME_NEVER_FREES: &[&str] = &[
    "__rucc_cap_witness",
    "__rucc_check_bounds",
    "__rucc_check_deriv",
    "__rucc_check_live",
    "__rucc_check_type",
    "__rucc_extent",
    "__rucc_extent_back",
    "__rucc_meta_type",
    "__rucc_meta_type_copy",
];

/// Whether either table vouches for that name, under any of the spellings it can arrive in.
///
/// `__builtin_memcpy` is the program saying which function it means. `__rucc_wrap_memcpy` is what
/// a call to `memcpy` becomes under `-fsafety`, and the wrapper checks the access and then calls
/// the function it wraps, so it ends whatever that one ends, which is nothing.
fn never_frees(name: &str) -> bool {
    if RUNTIME_NEVER_FREES.binary_search(&name).is_ok() {
        return true;
    }
    let name = name.strip_prefix(WRAPPER_PREFIX).unwrap_or(name);
    let name = name.strip_prefix("__builtin_").unwrap_or(name);
    NEVER_FREES.binary_search(&name).is_ok()
}

#[cfg(test)]
mod tests {
    use rucc_base::{Interner, Symbol};
    use rucc_ir::{
        Builder, CallInfo, Extra, Flags, Func, InstData, Linkage, Module, Opcode, Pic, Signature,
        Visibility,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::{NEVER_FREES, RUNTIME_NEVER_FREES, Summaries, annotate};

    /// What one function in a test module is.
    struct Def<'a> {
        /// Its name.
        name: &'a str,
        /// Whether it has a body. A function without one is a declaration.
        defined: bool,
        /// The functions it calls, in order.
        calls: &'a [&'a str],
    }

    /// A definition that calls those names.
    fn defines<'a>(name: &'a str, calls: &'a [&'a str]) -> Def<'a> {
        Def { name, defined: true, calls }
    }

    /// A declaration, which has no body and so calls nothing.
    fn declares(name: &str) -> Def<'_> {
        Def { name, defined: false, calls: &[] }
    }

    /// A module holding those functions.
    fn module(defs: &[Def<'_>]) -> (Interner, Module) {
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        for def in defs {
            let mut func = Func::new(names.intern(def.name), Signature::new());
            if def.defined {
                let block = func.create_block();
                let mut build = Builder::new(&mut func, block);
                let signature = build.func().add_signature(Signature::new());
                for call in def.calls {
                    build.call(names.intern(call), signature, &[]);
                }
                build.ret(&[]);
            }
            module.add_func(func);
        }
        (names, module)
    }

    /// Whether the summaries say that name cannot free.
    fn cannot_free(names: &mut Interner, module: &Module, name: &str) -> bool {
        let summaries = Summaries::of_module(module, names, Pic::Executable);
        summaries.cannot_free(names.intern(name))
    }

    /// Every call in the module that carries the flag, by the name it calls.
    fn marked(names: &Interner, module: &Module) -> Vec<String> {
        let mut found = Vec::new();
        for id in module.funcs() {
            let func = &module[id];
            for block in func.blocks() {
                for inst in func.insts(block) {
                    if !func[inst].flags.contains(Flags::NOFREE) {
                        continue;
                    }
                    let Extra::Call(at) = func[inst].extra else { continue };
                    let Some(callee) = func[at].callee else { continue };
                    found.push(names.resolve(callee).to_string());
                }
            }
        }
        found
    }

    #[test]
    fn a_function_that_calls_nothing_frees_nothing() {
        let (mut names, module) = module(&[defines("leaf", &[])]);
        assert!(cannot_free(&mut names, &module, "leaf"));
    }

    #[test]
    fn a_function_that_calls_free_can_free_and_so_can_its_callers() {
        let (mut names, module) = module(&[
            declares("free"),
            defines("releases", &["free"]),
            defines("above", &["releases"]),
        ]);
        assert!(!cannot_free(&mut names, &module, "free"));
        assert!(!cannot_free(&mut names, &module, "releases"));
        assert!(!cannot_free(&mut names, &module, "above"));
    }

    #[test]
    fn a_function_that_only_calls_nofree_ones_frees_nothing() {
        let (mut names, module) = module(&[
            declares("memcpy"),
            defines("leaf", &[]),
            defines("above", &["leaf", "memcpy"]),
        ]);
        assert!(cannot_free(&mut names, &module, "above"));
    }

    #[test]
    fn the_table_is_asked_about_a_name_the_module_has_no_function_for() {
        // Which is what a call `rucc-safety` put in looks like. It interned the name and emitted
        // the call, and there is no declaration anywhere in the module to go with it.
        let (mut names, module) = module(&[defines("above", &["__rucc_wrap_memcpy"])]);
        assert!(cannot_free(&mut names, &module, "__rucc_wrap_memcpy"));
        assert!(cannot_free(&mut names, &module, "above"));
    }

    #[test]
    fn a_name_the_module_has_no_function_for_and_the_table_does_not_know_can_still_free() {
        let (mut names, module) = module(&[defines("above", &["somebodys_free"])]);
        assert!(!cannot_free(&mut names, &module, "somebodys_free"));
        assert!(!cannot_free(&mut names, &module, "above"));
    }

    #[test]
    fn two_functions_that_call_each_other_and_free_nothing_are_both_nofree() {
        // Which is what starting optimistic and narrowing buys. Each waits on the other, so an
        // analysis that only ever added to the set would never put either of them in it.
        let (mut names, module) = module(&[defines("ping", &["pong"]), defines("pong", &["ping"])]);
        assert!(cannot_free(&mut names, &module, "ping"));
        assert!(cannot_free(&mut names, &module, "pong"));
    }

    #[test]
    fn a_cycle_with_a_free_anywhere_in_it_is_nofree_nowhere() {
        let (mut names, module) = module(&[
            declares("free"),
            defines("ping", &["pong"]),
            defines("pong", &["ping", "free"]),
        ]);
        assert!(!cannot_free(&mut names, &module, "ping"));
        assert!(!cannot_free(&mut names, &module, "pong"));
    }

    #[test]
    fn a_name_this_module_never_heard_of_can_free() {
        let (mut names, module) = module(&[defines("leaf", &[])]);
        assert!(!cannot_free(&mut names, &module, "elsewhere"));
    }

    #[test]
    fn the_library_table_is_read_under_every_spelling_a_name_arrives_in() {
        let (mut names, module) = module(&[
            declares("memcpy"),
            declares("__builtin_memcpy"),
            declares("__rucc_wrap_memcpy"),
            declares("realloc"),
        ]);
        assert!(cannot_free(&mut names, &module, "memcpy"));
        assert!(cannot_free(&mut names, &module, "__builtin_memcpy"));
        assert!(cannot_free(&mut names, &module, "__rucc_wrap_memcpy"));
        // `realloc` is the one that looks like the others and is not. It may free what it was
        // given, which is exactly the event the flag is about.
        assert!(!cannot_free(&mut names, &module, "realloc"));
    }

    #[test]
    fn a_definition_the_linker_may_replace_is_not_believed() {
        let (mut names, mut module) = module(&[defines("weakly", &[])]);
        let id = module.funcs().next().unwrap();
        module[id].linkage = Linkage::Weak;
        assert!(!cannot_free(&mut names, &module, "weakly"));
    }

    /// The whole of what `-fPIC` costs this analysis. A body it can read is one the dynamic linker
    /// may find a different definition of first, so the one in front of it is not the one that
    /// runs and nothing may be read off it.
    #[test]
    fn a_library_cannot_believe_a_body_something_else_may_replace() {
        let (mut names, module) = module(&[defines("exported", &[])]);
        let name = names.intern("exported");
        assert!(Summaries::of_module(&module, &names, Pic::Executable).cannot_free(name));
        assert!(!Summaries::of_module(&module, &names, Pic::Library).cannot_free(name));
    }

    /// And what it costs for a name nothing outside the library can reach, which is nothing. Both
    /// halves matter: the first is why `-fvisibility=hidden` is worth writing and the second is
    /// why a `static` helper is still believed in a library.
    #[test]
    fn a_library_believes_the_bodies_nothing_outside_it_can_name() {
        let (mut names, mut module) = module(&[defines("shy", &[]), defines("quiet", &[])]);
        let mut ids = module.funcs();
        let shy = ids.next().unwrap();
        let quiet = ids.next().unwrap();
        module[shy].visibility = Visibility::Hidden;
        module[quiet].linkage = Linkage::Internal;
        let summaries = Summaries::of_module(&module, &names, Pic::Library);
        assert!(summaries.cannot_free(names.intern("shy")));
        assert!(summaries.cannot_free(names.intern("quiet")));
    }

    #[test]
    fn a_call_through_an_address_could_reach_anything() {
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        let mut func = Func::new(names.intern("dispatch"), Signature::new());
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let signature = build.func().add_signature(Signature::new());
        let varargs = build.func().push_abis(&[]);
        let info = build.func().add_call(CallInfo { callee: None, signature, varargs });
        build.inst(
            InstData { extra: Extra::Call(info), ..InstData::new(Opcode::CallIndirect) },
            &[],
        );
        build.ret(&[]);
        module.add_func(func);
        assert!(!cannot_free(&mut names, &module, "dispatch"));
    }

    #[test]
    fn the_flag_goes_on_the_calls_the_summaries_vouch_for_and_no_others() {
        let (names, mut module) = module(&[
            declares("free"),
            declares("memcpy"),
            defines("leaf", &[]),
            defines("above", &["leaf", "memcpy", "free"]),
        ]);
        assert_eq!(annotate(&mut module, &names, Pic::Executable), 2);
        assert_eq!(marked(&names, &module), ["leaf", "memcpy"]);
        // Running it again finds nothing left to say, which is what makes it safe to run in a
        // pipeline that has already been through it once.
        assert_eq!(annotate(&mut module, &names, Pic::Executable), 0);
        assert_eq!(marked(&names, &module).len(), 2);
    }

    #[test]
    fn nothing_is_known_when_nothing_was_asked() {
        let empty = Summaries::nothing();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert!(!empty.cannot_free(Symbol::from_raw(0)));
    }

    #[test]
    fn the_library_table_is_sorted_and_says_each_name_once() {
        // Sorted because the lookup is a binary search, and each name once because an entry here
        // is believed without being checked against anything.
        for pair in NEVER_FREES.windows(2) {
            assert!(pair[0] < pair[1], "{} and {} are out of order", pair[0], pair[1]);
        }
        for &name in NEVER_FREES {
            assert!(!name.starts_with("__builtin_"), "{name} is reached under every spelling");
            assert!(!name.starts_with(super::WRAPPER_PREFIX), "{name} likewise");
        }
        for pair in RUNTIME_NEVER_FREES.windows(2) {
            assert!(pair[0] < pair[1], "{} and {} are out of order", pair[0], pair[1]);
        }
        // The four nobody should be tempted to add, written down so that adding one is a test
        // failure rather than a decision somebody makes alone. The last two are the runtime's own
        // and are the ones that do end a lifetime.
        assert!(!super::never_frees("realloc"));
        assert!(!super::never_frees("free"));
        assert!(!super::never_frees("__rucc_alloc_purge"));
        assert!(!super::never_frees("__rucc_frame_clear"));
    }

    #[test]
    fn the_runtime_entry_points_generated_code_calls_end_no_lifetime() {
        // The witness is the one a compile actually puts in front of the optimizer, at every place
        // a pointer crosses the instrumentation boundary, and it has no function in the module.
        let (mut names, module) = module(&[defines("above", &["__rucc_cap_witness"])]);
        assert!(cannot_free(&mut names, &module, "above"));
        for &name in RUNTIME_NEVER_FREES {
            assert!(super::never_frees(name), "{name} is in the table and not read from it");
        }
    }
}
