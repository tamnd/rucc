//! `--emit=safety-summary`: what this build's guarantee actually rests on.
//!
//! Design: `spec/safe-memory/07-check-elimination.md` section 7.8, and
//! `spec/safe-memory/10-boundaries.md` section 10.2 for why it is a headline artifact rather than
//! a debugging convenience.
//!
//! Every safety argument has a trust set: the things its claim depends on and does not check. The
//! contribution here is not having a small one, which everybody claims, but counting ours per
//! build, so that "this binary's guarantee rests on two unwrapped calls and one asm site" is a
//! sentence somebody can read off an artifact rather than a sentence somebody asserts. ASan does
//! not tell you how much of your program it did not instrument. This does.
//!
//! A summary is per translation unit, because that is what a compiler sees. The build system is
//! what adds them up, and the schema is stable so that adding them up is a script rather than a
//! parser.
//!
//! Two things this deliberately does not do.
//!
//! It does not print zero for a row that can only be counted while the program runs. The number of
//! capabilities recovered at a boundary is a property of an execution, not of a translation unit,
//! and a summary that printed `"recovered": 0` for a file that will recover ten thousand of them
//! at run time would be worse than saying nothing. The names to read that count out of a running
//! program are listed instead.
//!
//! It does not attribute a discharge to the rule that made it. Section 7.8 asks for that too, and
//! for it to mean anything the optimizer has to record which rule removed which check, which is
//! milestone S4's work rather than this one's. What is here is the counts, which is the half the
//! milestone's exit criterion needs and the half document 13's cost model consumes.
//!
//! The frame counts are the one row here that is a prediction rather than a fact. Document 13
//! section 13.5 wants the call frame elision rate as the fraction of instrumented calls where the
//! frame is dropped, and today nothing publishes a frame at all: `rucc-safe-rt` has the reader
//! side and the compiler emits no writer, so the honest run time rate is undefined rather than
//! zero. What this counts instead is how often the rule of document 05 section 5.3 would fire, per
//! call site, on the module the optimizer just finished with. That is the number document 17
//! question 4 is actually asking for, since the question is whether the rule is worth building,
//! and it can be answered before the frame exists.

use std::collections::HashMap;

use rucc_base::{Interner, Symbol};
use rucc_ir::{Extra, Inst, Module, Opcode};

use crate::Counts;
use crate::boundary::Sites;
use crate::wrap::INTERPOSED;

/// The version of the schema the JSON below is written in.
///
/// Bumped when a field changes meaning or goes away, not when one is added, which is the usual
/// contract and the one a consumer can rely on without pinning a compiler version.
pub const SCHEMA: u32 = 1;

/// The checks of one class: how many went in, and how many are still there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Class {
    /// How many the insertion pass put in.
    pub emitted: usize,
    /// How many survived the optimizer.
    pub remaining: usize,
}

impl Class {
    /// How many the optimizer proved it did not need.
    ///
    /// Saturating, because a pass that somehow added checks should show as zero discharged rather
    /// than as an arithmetic panic in a reporting path.
    #[must_use]
    pub const fn discharged(self) -> usize {
        self.emitted.saturating_sub(self.remaining)
    }
}

/// Call sites sorted by whether the capability frame around them could be dropped.
///
/// Document 05 section 5.3 charges every instrumented call one TLS access and up to eight
/// capability stores, and says the charge goes away when the callee is in this module and has no
/// checks left. The four buckets below are that rule read off a call site: one says it fires, and
/// the other three are the three reasons it does not.
///
/// Every call in the unit lands in exactly one of the five numbers, so they add up and a reader
/// can see the denominator instead of taking a percentage on faith. That includes calls to this
/// compiler's own interposition wrappers, which are counted as `outside`, because a wrapper really
/// is a function in another translation unit whose checks this build cannot see.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Frames {
    /// The callee is defined here and has no checks left, so it never reads a frame.
    pub elided: usize,
    /// The callee is defined here and still checks something, so it needs the capabilities.
    pub checked: usize,
    /// The callee is not defined here, so nothing in this unit knows what it checks.
    pub outside: usize,
    /// The call goes through a pointer, so there is no callee to ask.
    pub unknown: usize,
    /// The call hands no pointer over, so there was never a capability to pass.
    pub pointerless: usize,
}

impl Frames {
    /// Calls that would carry a frame, which is the denominator of the rate.
    #[must_use]
    pub const fn wanted(self) -> usize {
        self.elided + self.checked + self.outside + self.unknown
    }
}

/// Everything one translation unit has to say about its own safety.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// The file this is about.
    pub unit: String,
    /// The tier it was built at, as `-fsafety=` spells it.
    pub tier: &'static str,
    /// Bounds checks, judgement J1.
    pub bounds: Class,
    /// Lifetime checks, the other half of J1.
    pub lifetime: Class,
    /// Derivation checks, judgement J2.
    pub derivation: Class,
    /// Type checks, which is the effective type rule of C 6.5 asked of the type plane.
    ///
    /// Fewer than `bounds`, because only a read asks and only a read the front end named a type
    /// for has a question to put. `crate::ask` is where both of those are argued.
    pub effective_type: Class,
    /// Initialization checks, which is document 03's Y6 asked of the init plane.
    ///
    /// The same count as the reads in `bounds`, because the question is whether the bytes hold
    /// anything at all and that is a question about every read. `crate::filled` is where the
    /// difference from `effective_type` is argued.
    pub initialization: Class,
    /// Accesses that got no check at all, because the pointer they go through is not a value the
    /// insertion pass can take the capability of. A hole rather than a discharge, which is why it
    /// is not folded into either number above.
    pub unchecked: usize,
    /// Calls that were pointed at an interposition wrapper.
    pub interposed: usize,
    /// How many rows the table has, so that a reader can tell "no calls to interposed functions"
    /// apart from "no interposition table in this build".
    pub rows: usize,
    /// Names this unit calls, does not define, and has no wrapper for. Section 10.2's unwrapped
    /// symbol list, which is the boundary this build did not model, by name.
    pub external: Vec<String>,
    /// Calls through a pointer, which cannot be redirected because nothing at the call site says
    /// which function the address names.
    pub indirect: usize,
    /// Pointers turned into integers, which is document 04 section 4.3's exposure.
    pub exposed: usize,
    /// Integers turned into pointers, which is judgement J3.
    pub synthesized: usize,
    /// Inline assembly sites, each of which is trusted to do what its constraints say.
    pub asm: usize,
    /// Places a pointer crosses between this build and code it did not instrument, which is what
    /// the run time recovery counts will be counting.
    pub crossings: Sites,
    /// Whether the capability frame around each call site could be dropped.
    pub frames: Frames,
}

impl Summary {
    /// The JSON of section 7.8, as one string.
    ///
    /// Written by hand rather than through a serializer, because the schema is a dozen fields and
    /// a stable schema is easier to keep stable when the bytes are in front of you. Two spaces of
    /// indentation and a trailing newline, so that a diff of two summaries reads.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("{\n");
        out.push_str(&format!("  \"schema\": {SCHEMA},\n"));
        out.push_str(&format!("  \"unit\": {},\n", quoted(&self.unit)));
        out.push_str(&format!("  \"tier\": {},\n", quoted(self.tier)));
        out.push_str("  \"checks\": {\n");
        out.push_str(&format!("    \"bounds\": {},\n", class(self.bounds)));
        out.push_str(&format!("    \"lifetime\": {},\n", class(self.lifetime)));
        out.push_str(&format!("    \"derivation\": {},\n", class(self.derivation)));
        out.push_str(&format!("    \"type\": {},\n", class(self.effective_type)));
        out.push_str(&format!("    \"init\": {},\n", class(self.initialization)));
        out.push_str(&format!("    \"unchecked\": {}\n", self.unchecked));
        out.push_str("  },\n");
        out.push_str("  \"trust\": {\n");
        out.push_str(&format!("    \"interposed\": {},\n", self.interposed));
        out.push_str(&format!("    \"rows\": {},\n", self.rows));
        out.push_str(&format!("    \"external\": {},\n", list(&self.external)));
        out.push_str(&format!("    \"indirect\": {},\n", self.indirect));
        out.push_str(&format!("    \"exposed\": {},\n", self.exposed));
        out.push_str(&format!("    \"synthesized\": {},\n", self.synthesized));
        out.push_str(&format!("    \"asm\": {},\n", self.asm));
        out.push_str(&format!(
            "    \"crossings\": {{ \"entered\": {}, \"returned\": {} }}\n",
            self.crossings.entered, self.crossings.returned
        ));
        out.push_str("  },\n");
        out.push_str("  \"frames\": {\n");
        out.push_str(&format!("    \"elided\": {},\n", self.frames.elided));
        out.push_str(&format!("    \"checked\": {},\n", self.frames.checked));
        out.push_str(&format!("    \"outside\": {},\n", self.frames.outside));
        out.push_str(&format!("    \"unknown\": {},\n", self.frames.unknown));
        out.push_str(&format!("    \"pointerless\": {},\n", self.frames.pointerless));
        // The denominator, so that adding up two units is adding up six numbers rather than
        // reconstructing which four of the five belong on the bottom of the fraction.
        out.push_str(&format!("    \"wanted\": {}\n", self.frames.wanted()));
        out.push_str("  },\n");
        // Named rather than counted, for the reason the module comment gives.
        out.push_str("  \"at_run_time\": [\n");
        out.push_str("    \"__rucc_safety_recovered\",\n");
        out.push_str("    \"__rucc_safety_recovered_wide\"\n");
        out.push_str("  ]\n");
        out.push_str("}\n");
        out
    }
}

/// Walks a module after the optimizer and says what is left, which is most of a [`Summary`].
///
/// `emitted` is what [`crate::run`] counted on the way in and `interposed` is what
/// [`crate::redirect`] moved, because neither is recoverable from the module afterwards: a check
/// the optimizer discharged leaves nothing behind saying it was ever there, which is the whole
/// reason those two functions return a number.
#[must_use]
pub fn summarize(
    module: &Module,
    names: &Interner,
    unit: &str,
    tier: &'static str,
    emitted: Counts,
    interposed: usize,
    crossings: Sites,
) -> Summary {
    let mut summary = Summary {
        unit: unit.to_string(),
        tier,
        bounds: Class { emitted: emitted.checked, remaining: 0 },
        lifetime: Class { emitted: emitted.live, remaining: 0 },
        derivation: Class { emitted: emitted.derived, remaining: 0 },
        effective_type: Class { emitted: emitted.asked, remaining: 0 },
        initialization: Class { emitted: emitted.filled, remaining: 0 },
        unchecked: emitted.skipped,
        interposed,
        rows: INTERPOSED.len(),
        crossings,
        ..Summary::default()
    };

    // What every function this unit defines still has to check, which is what decides whether a
    // call into it wants a frame. It is its own pass because a callee is allowed to be defined
    // after its caller and the answer has to be the same either way.
    let left: HashMap<Symbol, usize> = module
        .funcs()
        .filter(|&id| !module[id].is_declaration())
        .map(|id| (module[id].name, checks_left(&module[id])))
        .collect();
    // The wrappers are ours and are not the boundary this build failed to model, so they do not
    // belong on the unwrapped list even though every one of them is an undefined symbol here.
    let mut external: Vec<Symbol> = Vec::new();

    for id in module.funcs() {
        if module[id].is_declaration() {
            continue;
        }
        let func = &module[id];
        let insts: Vec<Inst> =
            func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
        for inst in insts {
            match func[inst].opcode {
                Opcode::CheckBounds => summary.bounds.remaining += 1,
                Opcode::CheckLive => summary.lifetime.remaining += 1,
                Opcode::CheckDeriv => summary.derivation.remaining += 1,
                Opcode::CheckType => summary.effective_type.remaining += 1,
                Opcode::CheckInit => summary.initialization.remaining += 1,
                Opcode::PtrToInt => summary.exposed += 1,
                Opcode::IntToPtr => summary.synthesized += 1,
                Opcode::InlineAsm => summary.asm += 1,
                Opcode::Call | Opcode::TailCall | Opcode::CallIndirect => {
                    let indirect = func[inst].opcode == Opcode::CallIndirect;
                    if indirect {
                        summary.indirect += 1;
                    }
                    // An indirect call has no callee to read, and reading one anyway would put a
                    // name on the unwrapped list for a call that does not go there.
                    let callee = match func[inst].extra {
                        Extra::Call(at) if !indirect => func[at].callee,
                        _ => None,
                    };
                    match callee {
                        Some(callee)
                            if !left.contains_key(&callee)
                                && !external.contains(&callee)
                                && !ours(names.resolve(callee)) =>
                        {
                            external.push(callee);
                        }
                        Some(_) => {}
                        // A call with no callee is one through a pointer that reached here as a
                        // `Call` rather than a `CallIndirect`, and it is the same trust question.
                        None if !indirect => summary.indirect += 1,
                        None => {}
                    }
                    // The first operand of an indirect call is the address it jumps to, which is
                    // a pointer the callee never receives, so it is not one the frame would hold.
                    let skip = usize::from(indirect);
                    let hands_over = func[func[inst].args]
                        .iter()
                        .skip(skip)
                        .any(|&value| func[value].ty.is_ptr());
                    if !hands_over {
                        summary.frames.pointerless += 1;
                    } else {
                        match callee.and_then(|callee| left.get(&callee)) {
                            None => {
                                if indirect || callee.is_none() {
                                    summary.frames.unknown += 1;
                                } else {
                                    summary.frames.outside += 1;
                                }
                            }
                            Some(0) => summary.frames.elided += 1,
                            Some(_) => summary.frames.checked += 1,
                        }
                    }
                }
                _ => {}
            }
        }
    }

    summary.external = external.iter().map(|&s| names.resolve(s).to_string()).collect();
    // Sorted, so that two builds of the same file produce the same bytes. The walk order is the
    // function order in the module, which is a thing the front end is allowed to change.
    summary.external.sort_unstable();
    summary
}

/// How many checks of any class one function still has standing.
///
/// A function with none of them never reads the frame its callers set up, which is the whole
/// condition document 05 section 5.3 puts on dropping it.
fn checks_left(func: &rucc_ir::Func) -> usize {
    let insts: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    insts
        .into_iter()
        .filter(|&inst| {
            matches!(
                func[inst].opcode,
                Opcode::CheckBounds
                    | Opcode::CheckLive
                    | Opcode::CheckDeriv
                    | Opcode::CheckType
                    | Opcode::CheckInit
            )
        })
        .count()
}

/// Whether a name is one this compiler put there rather than one the program called.
///
/// The wrappers and the check entry points are undefined symbols in every instrumented object and
/// none of them is a boundary the build failed to model, so counting them would make an
/// instrumented file look less trustworthy than the uninstrumented one it came from, which is
/// exactly backwards.
fn ours(name: &str) -> bool {
    name.starts_with("__rucc_")
}

/// One class as a JSON object, on one line, because three numbers do not need three lines.
fn class(class: Class) -> String {
    format!(
        "{{ \"emitted\": {}, \"remaining\": {}, \"discharged\": {} }}",
        class.emitted,
        class.remaining,
        class.discharged()
    )
}

/// A list of names as a JSON array, on one line when it is empty.
fn list(names: &[String]) -> String {
    if names.is_empty() {
        return "[]".to_string();
    }
    let mut out = String::from("[\n");
    for (at, name) in names.iter().enumerate() {
        let comma = if at + 1 == names.len() { "" } else { "," };
        out.push_str(&format!("      {}{comma}\n", quoted(name)));
    }
    out.push_str("    ]");
    out
}

/// A JSON string, with the five escapes a file name or a C identifier can actually contain.
///
/// A path may hold a backslash on Windows and a quote on anything, and a control character in a
/// path is unusual rather than impossible. Everything else is passed through as its own bytes,
/// which is valid JSON: the format is defined over text and a UTF-8 string needs no escaping to
/// be one.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use rucc_ir::{Builder, Func, InstData, Linkage, MemInfo, MemOrder, Restrict, Signature, Type};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::*;
    use crate::insert;

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// One function that loads through its pointer parameter, with checks put in.
    fn guarded(names: &mut Interner) -> Func {
        let i32_ = Type::int(32);
        let mut func = Func::new(
            names.intern("guarded"),
            Signature::new().with_params(&[Type::PTR]).with_returns(&[i32_]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let info = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        let loaded = b.value(InstData { args, extra, ..InstData::new(Opcode::Load) }, i32_);
        b.ret(&[loaded]);
        // The function reads and never stores, so it records nothing and the plane it is
        // instrumented against is one with no types in it.
        let mut elsewhere = Module::new(names.intern("reader.c"), &target());
        insert(&mut func, &crate::Plane::build(&mut elsewhere));
        func
    }

    /// One function that takes a pointer and never looks at it, so it has nothing to check.
    fn clean(names: &mut Interner) -> Func {
        let i32_ = Type::int(32);
        let mut func = Func::new(
            names.intern("clean"),
            Signature::new().with_params(&[Type::PTR]).with_returns(&[i32_]),
        );
        let entry = func.create_block();
        func.append_param(entry, Type::PTR);
        let mut b = Builder::new(&mut func, entry);
        let zero = b.iconst(i32_, 0);
        b.ret(&[zero]);
        func
    }

    /// A module whose `main` calls all four kinds of callee the frame rule distinguishes.
    fn calls(names: &mut Interner) -> Module {
        let i32_ = Type::int(32);
        let taking = Signature::new().with_params(&[Type::PTR]).with_returns(&[i32_]);
        let nothing = Signature::new().with_returns(&[i32_]);

        let mut func = Func::new(names.intern("main"), taking.clone());
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let taking = func.add_signature(taking);
        let nothing = func.add_signature(nothing);
        let no_checks = names.intern("clean");
        let has_checks = names.intern("guarded");
        let elsewhere = names.intern("puts");
        let ticks = names.intern("ticks");
        let mut b = Builder::new(&mut func, entry);
        b.call(no_checks, taking, &[p]);
        b.call(has_checks, taking, &[p]);
        b.call(elsewhere, taking, &[p]);
        b.call(ticks, nothing, &[]);
        let zero = b.iconst(i32_, 0);
        b.ret(&[zero]);

        let mut module = Module::new(names.intern("calls.c"), &target());
        module.add_func(clean(names));
        module.add_func(guarded(names));
        module.add_func(func);
        let mut declared = Func::new(elsewhere, Signature::new().with_params(&[Type::PTR]));
        declared.linkage = Linkage::External;
        module.add_func(declared);
        module
    }

    fn frames_of(module: &Module, names: &Interner) -> Frames {
        summarize(module, names, "calls.c", "detect", Counts::default(), 0, Sites::default()).frames
    }

    /// A summary with a couple of numbers in it, to render.
    fn filled() -> Summary {
        Summary {
            unit: "a.c".to_string(),
            tier: "detect",
            bounds: Class { emitted: 12, remaining: 5 },
            lifetime: Class { emitted: 12, remaining: 11 },
            derivation: Class { emitted: 3, remaining: 3 },
            effective_type: Class { emitted: 7, remaining: 7 },
            initialization: Class { emitted: 9, remaining: 8 },
            unchecked: 1,
            interposed: 2,
            rows: 27,
            external: vec!["printf".to_string(), "qsort".to_string()],
            indirect: 1,
            exposed: 0,
            synthesized: 0,
            asm: 1,
            crossings: Sites { entered: 2, returned: 1 },
            frames: Frames { elided: 4, checked: 2, outside: 3, unknown: 1, pointerless: 5 },
        }
    }

    #[test]
    fn a_discharged_check_is_one_that_went_in_and_is_not_there_now() {
        assert_eq!(Class { emitted: 12, remaining: 5 }.discharged(), 7);
        assert_eq!(Class { emitted: 0, remaining: 0 }.discharged(), 0);
    }

    #[test]
    fn a_pass_that_somehow_added_checks_discharges_none_rather_than_panicking() {
        assert_eq!(Class { emitted: 1, remaining: 4 }.discharged(), 0);
    }

    #[test]
    fn the_summary_says_the_schema_it_is_written_in() {
        let text = filled().render();
        assert!(text.contains("\"schema\": 1"), "{text}");
    }

    #[test]
    fn every_class_reports_all_three_numbers() {
        let text = filled().render();
        assert!(
            text.contains("\"bounds\": { \"emitted\": 12, \"remaining\": 5, \"discharged\": 7 }"),
            "{text}"
        );
        assert!(
            text.contains(
                "\"derivation\": { \"emitted\": 3, \"remaining\": 3, \"discharged\": 0 }"
            ),
            "{text}"
        );
        assert!(
            text.contains("\"type\": { \"emitted\": 7, \"remaining\": 7, \"discharged\": 0 }"),
            "{text}"
        );
        assert!(
            text.contains("\"init\": { \"emitted\": 9, \"remaining\": 8, \"discharged\": 1 }"),
            "{text}"
        );
    }

    #[test]
    fn the_unwrapped_calls_are_named_rather_than_counted() {
        let text = filled().render();
        assert!(text.contains("\"printf\""), "{text}");
        assert!(text.contains("\"qsort\""), "{text}");
    }

    #[test]
    fn a_unit_with_nothing_to_hide_says_so_with_an_empty_list() {
        let text = Summary { external: Vec::new(), ..filled() }.render();
        assert!(text.contains("\"external\": [],"), "{text}");
    }

    #[test]
    fn the_boundary_crossings_are_counted_by_direction() {
        // A pointer arriving is a function of this build somebody else can call, and a pointer
        // coming back is a library this build chose to link against. One number would hide which
        // of the two a program is made of.
        let text = filled().render();
        assert!(text.contains("\"crossings\": { \"entered\": 2, \"returned\": 1 }"), "{text}");
    }

    #[test]
    fn the_run_time_counts_are_named_rather_than_guessed_at() {
        let text = filled().render();
        assert!(text.contains("__rucc_safety_recovered"), "{text}");
        assert!(!text.contains("\"recovered\": 0"), "{text}");
    }

    #[test]
    fn a_name_with_a_quote_in_it_comes_out_as_json_rather_than_as_two_strings() {
        let text = Summary { unit: "a\"b\\c.c".to_string(), ..filled() }.render();
        assert!(text.contains(r#""unit": "a\"b\\c.c""#), "{text}");
    }

    #[test]
    fn a_call_into_a_function_with_no_checks_left_is_one_whose_frame_goes_away() {
        let mut names = Interner::new();
        let module = calls(&mut names);
        let frames = frames_of(&module, &names);
        assert_eq!(frames.elided, 1, "{frames:?}");
        assert_eq!(frames.checked, 1, "{frames:?}");
        assert_eq!(frames.outside, 1, "{frames:?}");
        assert_eq!(frames.unknown, 0, "{frames:?}");
    }

    #[test]
    fn a_call_that_hands_no_pointer_over_never_wanted_a_frame() {
        let mut names = Interner::new();
        let module = calls(&mut names);
        let frames = frames_of(&module, &names);
        assert_eq!(frames.pointerless, 1, "{frames:?}");
        assert_eq!(frames.wanted(), 3, "{frames:?}");
    }

    #[test]
    fn the_five_buckets_account_for_every_call_in_the_unit() {
        let mut names = Interner::new();
        let module = calls(&mut names);
        let frames = frames_of(&module, &names);
        assert_eq!(frames.wanted() + frames.pointerless, 4, "{frames:?}");
    }

    #[test]
    fn the_rate_is_reported_with_its_denominator_beside_it() {
        let text = filled().render();
        assert!(text.contains("\"elided\": 4"), "{text}");
        assert!(text.contains("\"wanted\": 10"), "{text}");
    }

    #[test]
    fn the_whole_thing_is_one_object_and_ends_in_a_newline() {
        let text = filled().render();
        assert!(text.starts_with("{\n"), "{text}");
        assert!(text.ends_with("}\n"), "{text}");
        assert_eq!(text.matches('{').count(), text.matches('}').count(), "{text}");
    }
}
