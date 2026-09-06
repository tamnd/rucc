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

use rucc_base::{Interner, Symbol};
use rucc_ir::{Extra, Inst, Module, Opcode};

use crate::Counts;
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
}

impl Summary {
    /// The JSON of section 7.8, as one string.
    ///
    /// Written by hand rather than through a serializer, because the schema is eleven fields and
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
        out.push_str(&format!("    \"unchecked\": {}\n", self.unchecked));
        out.push_str("  },\n");
        out.push_str("  \"trust\": {\n");
        out.push_str(&format!("    \"interposed\": {},\n", self.interposed));
        out.push_str(&format!("    \"rows\": {},\n", self.rows));
        out.push_str(&format!("    \"external\": {},\n", list(&self.external)));
        out.push_str(&format!("    \"indirect\": {},\n", self.indirect));
        out.push_str(&format!("    \"exposed\": {},\n", self.exposed));
        out.push_str(&format!("    \"synthesized\": {},\n", self.synthesized));
        out.push_str(&format!("    \"asm\": {}\n", self.asm));
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
) -> Summary {
    let mut summary = Summary {
        unit: unit.to_string(),
        tier,
        bounds: Class { emitted: emitted.checked, remaining: 0 },
        lifetime: Class { emitted: emitted.live, remaining: 0 },
        derivation: Class { emitted: emitted.derived, remaining: 0 },
        unchecked: emitted.skipped,
        interposed,
        rows: INTERPOSED.len(),
        ..Summary::default()
    };

    let defined: Vec<Symbol> = module
        .funcs()
        .filter(|&id| !module[id].is_declaration())
        .map(|id| module[id].name)
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
                Opcode::PtrToInt => summary.exposed += 1,
                Opcode::IntToPtr => summary.synthesized += 1,
                Opcode::InlineAsm => summary.asm += 1,
                Opcode::CallIndirect => summary.indirect += 1,
                Opcode::Call | Opcode::TailCall => {
                    let Extra::Call(at) = func[inst].extra else { continue };
                    match func[at].callee {
                        Some(callee)
                            if !defined.contains(&callee)
                                && !external.contains(&callee)
                                && !ours(names.resolve(callee)) =>
                        {
                            external.push(callee);
                        }
                        Some(_) => {}
                        // A call with no callee is one through a pointer that reached here as a
                        // `Call` rather than a `CallIndirect`, and it is the same trust question.
                        None => summary.indirect += 1,
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
    use super::*;

    /// A summary with a couple of numbers in it, to render.
    fn filled() -> Summary {
        Summary {
            unit: "a.c".to_string(),
            tier: "detect",
            bounds: Class { emitted: 12, remaining: 5 },
            lifetime: Class { emitted: 12, remaining: 11 },
            derivation: Class { emitted: 3, remaining: 3 },
            unchecked: 1,
            interposed: 2,
            rows: 27,
            external: vec!["printf".to_string(), "qsort".to_string()],
            indirect: 1,
            exposed: 0,
            synthesized: 0,
            asm: 1,
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
    fn the_whole_thing_is_one_object_and_ends_in_a_newline() {
        let text = filled().render();
        assert!(text.starts_with("{\n"), "{text}");
        assert!(text.ends_with("}\n"), "{text}");
        assert_eq!(text.matches('{').count(), text.matches('}').count(), "{text}");
    }
}
