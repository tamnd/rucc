//! What the floating point flags describe, which is arithmetic nothing rearranges.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.6.
//!
//! `-ffp-contract=`, `-frounding-math`, `-ftrapping-math` and `-fexcess-precision=` are four ways
//! of asking the same kind of question: how much liberty the compiler may take with an arithmetic
//! the program wrote. Each has a restrictive spelling and a permissive one, and this compiler sits
//! on the restrictive side of all four. It fuses no multiply into an addition, folds no floating
//! point arithmetic in a function body, reassociates nothing, and computes every operation in the
//! type it was written in. So the restrictive spellings describe what already happens and the
//! permissive ones are licences taken and not used, which is the same shape of answer
//! `-fno-strict-aliasing` and `-fno-delete-null-pointer-checks` get.
//!
//! That is only allowed to be the answer while it is true, which is what this file is for. Each
//! shape below is one gcc 16 rearranges under the permissive spelling and leaves alone under the
//! restrictive one, and the day one of these stops coming out whole, whoever made that change has
//! to make the flags turn it off in the same change. That is the rule
//! `spec/04-driver-and-cli.md` section 4.1 states for exactly this case.
//!
//! The assembly is asserted and not only the IR, because fusing is a thing a code generator does
//! rather than a thing a pass does: a machine with an `fma` instruction can emit one for a multiply
//! and an addition that are still two instructions right up until they are selected. Only x86-64 is
//! asked, because it is the only target this compiler has a back end for.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the mnemonics are the same
/// wherever this runs.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The shapes, each of which some spelling of these flags lets gcc 16 answer with something other
/// than the instructions that were written.
///
/// The first two are the contraction itself, in both widths, since a machine has an `fma` for each
/// and a code generator that formed one might form only one. The third and fourth are what
/// `-frounding-math` and `-ftrapping-math` are about: an addition folded at compile time is folded
/// in the default rounding mode whatever the program set, and a division by zero folded at compile
/// time is an exception the program never sees raised. The fifth is reassociation, which needs the
/// sum to be exact to be worth anything and is not. The last two are the identities that hold for
/// real numbers and not for floating point, because zero has a sign and because a NaN is not equal
/// to itself. The declaration at the end is not a shape at all and is here so that the module has
/// a function with no body in it, which is the one thing `-ffp-contract=` must not write on.
const SHAPES: &str = "\
double fma_double(double a, double b, double c) { return a * b + c; }
float fma_float(float a, float b, float c) { return a * b + c; }
double folds(void) { return 0.1 + 0.2; }
double divides_by_zero(void) { return 1.0 / 0.0; }
double reassociates(double a, double b, double c) { return (a + b) + c - b; }
double times_zero(double a) { return a * 0.0; }
double minus_itself(double a) { return a - a; }
extern double outside(double);
double calls(double a) { return outside(a); }
";

/// The mnemonics that are a fused multiply and addition on this target, in every sign and in both
/// the scalar and the packed form, since a vectorizer reaching one would reach the packed one.
const FUSED: &[&str] = &["vfmadd", "vfmsub", "vfnmadd", "vfnmsub", "vfmaddsub", "vfmsubadd"];

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-fp-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SHAPES).expect("the fixture can be written");
    path
}

/// What the compiler produced for the shapes at that level under those flags, in whichever form
/// `emit` asked for.
fn emit(what: &str, emit: &str, level: &str, flags: &[&str]) -> String {
    let path = fixture(what);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args([level, &format!("--emit={emit}")])
        .args(["-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(
        out.status.success(),
        "the compiler refused {flags:?}:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The instructions of one function, without the name of it or the braces around it.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("func @{name}(");
    text.lines()
        .map(str::trim)
        .skip_while(|line| !line.starts_with(&open))
        .skip(1)
        .take_while(|line| *line != "}")
        .filter(|line| !line.is_empty())
        .collect()
}

/// The instruction each line of assembly is, which is its first word, so that a mnemonic is not
/// found inside a symbol name or a comment.
fn mnemonics(asm: &str) -> Vec<&str> {
    asm.lines().filter_map(|line| line.split_whitespace().next()).collect()
}

#[test]
fn no_multiply_and_addition_is_ever_fused() {
    // Every value of the flag, since the permissive ones are the ones a fusing compiler would act
    // on, and `-march=haswell` beside them because that is the machine that has the instruction:
    // the default machine could not emit one whatever it was asked, so asking only the default
    // would prove nothing.
    let machines: &[&[&str]] = &[&[], &["-march=haswell"], &["-march=x86-64-v3"]];
    for contract in ["-ffp-contract=off", "-ffp-contract=on", "-ffp-contract=fast"] {
        for machine in machines {
            for level in ["-O0", "-O1", "-O2", "-O3"] {
                let mut flags = vec![contract];
                flags.extend_from_slice(machine);
                let asm = emit("fuse", "asm", level, &flags);
                let found: Vec<&str> = mnemonics(&asm)
                    .into_iter()
                    .filter(|m| FUSED.iter().any(|fused| m.starts_with(fused)))
                    .collect();
                assert!(found.is_empty(), "{contract} {machine:?} {level}: {found:?}");

                // And the two operations are still two in the IR, which is what says the assembly
                // has none because nothing fused them rather than because nothing reached them.
                let ir = emit("fuse-ir", "ir", level, &flags);
                for name in ["fma_double", "fma_float"] {
                    let body = body(&ir, name);
                    assert!(
                        body.iter().any(|line| line.contains("fmul"))
                            && body.iter().any(|line| line.contains("fadd")),
                        "{contract} {machine:?} {level} on {name}: {body:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn arithmetic_the_rounding_mode_would_change_is_still_done_at_run_time() {
    // Both spellings of both flags, because the permissive ones are permission to fold and the
    // question is whether anything takes it. gcc folds all four of these under
    // `-fno-trapping-math`, and the first two under its default, which is why the restrictive
    // spellings exist at all.
    let sets: &[&[&str]] = &[
        &[],
        &["-frounding-math", "-ftrapping-math"],
        &["-fno-rounding-math", "-fno-trapping-math"],
        &["-fno-rounding-math", "-fno-trapping-math", "-fexcess-precision=fast"],
    ];
    for flags in sets {
        for level in ["-O0", "-O1", "-O2", "-O3"] {
            let ir = emit("fold", "ir", level, flags);
            for (name, op) in [
                ("folds", "fadd"),
                ("divides_by_zero", "fdiv"),
                ("times_zero", "fmul"),
                ("minus_itself", "fsub"),
            ] {
                let body = body(&ir, name);
                assert!(
                    body.iter().any(|line| line.contains(op)),
                    "{flags:?} {level} on {name}: {body:?}"
                );
            }

            // And reassociation, which is not one instruction surviving but four: a compiler that
            // took `(a + b) + c - b` for `a + c` would be down to two.
            let body = body(&ir, "reassociates");
            let arithmetic =
                body.iter().filter(|line| line.contains("fadd") || line.contains("fsub")).count();
            assert_eq!(arithmetic, 3, "{flags:?} {level}: {body:?}");
        }
    }
}

/// And the flags change nothing about what comes out, except for the one that is recorded.
///
/// Asserted as the whole module being the same rather than as the shapes surviving, because a
/// compiler where one of these did something would have two answers and this has one. The three
/// that are descriptions are compared as they are. `-ffp-contract=` is compared with the attribute
/// it sets taken off each function, since that attribute is the whole of what it does and comparing
/// with it in would be asserting that it does nothing at all.
#[test]
fn nothing_but_the_recorded_flag_changes_what_comes_out() {
    let module = |what: &str, flags: &[&str]| {
        let text = emit(what, "ir", "-O2", flags);
        text.lines().filter(|line| !line.starts_with("; ModuleID")).collect::<Vec<_>>().join("\n")
    };
    let plain = module("plain", &[]);
    for flags in [
        vec!["-frounding-math"],
        vec!["-fno-rounding-math"],
        vec!["-ftrapping-math"],
        vec!["-fno-trapping-math"],
        vec!["-fexcess-precision=standard"],
        vec!["-fexcess-precision=fast"],
        vec!["-fexcess-precision=16"],
    ] {
        assert_eq!(module("describes", &flags), plain, "{flags:?}");
    }

    let without_attrs = |text: &str| {
        text.lines()
            .map(|line| match line.find(", attrs(") {
                Some(at) if line.starts_with("func @") => line[..at].to_string() + " {",
                _ => line.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let bare = without_attrs(&plain);
    for how in ["off", "on", "fast"] {
        let text = module("contract", &[&format!("-ffp-contract={how}")]);
        assert_eq!(without_attrs(&text), bare, "{how}");
    }

    // And what it does is recorded, on a function with a body and not on a declaration, since a
    // licence about code that is not in this file would be a claim about somebody else's.
    let text = module("recorded", &["-ffp-contract=fast"]);
    let written: Vec<&str> =
        text.lines().filter(|line| line.contains("fp_contract=fast")).collect();
    assert_eq!(written.len(), 8, "one per function with a body: {written:?}");
    assert!(
        text.lines().any(|line| line.starts_with("func @outside(") && !line.contains("attrs")),
        "the declaration was written on: {text}"
    );
}
