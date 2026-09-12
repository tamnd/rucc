//! Arithmetic at binary128, held against the system compiler by running it.
//!
//! Design: `spec/12-abi-and-runtime.md` section 12.8, which is where the routines an operation at
//! this format calls are, and where what runs them is.
//!
//! `crates/rucc-codegen/src/quad.rs` is the pass that makes this format work: no machine has an
//! instruction that adds two binary128 values, so every operation at it becomes a call to one of the
//! twenty four routines `runtime/builtins/quad.c` defines. What checked that until now was the pass's
//! own tests, which build a function, run the pass and read the IR back. That is the same kind of
//! evidence as a stub writer reading its own bytes back: it catches a pass that did something other
//! than what it meant to and not a pass that meant the wrong thing. Nothing ran the code.
//!
//! The routines themselves are checked a layer down. `cargo xtask builtins-diff` holds every one of
//! them against a Rust reference over eight million comparisons, and that is about the arithmetic
//! being right. This is about the call being made: the right routine, the operands the right way
//! round, and the answer read back the way the routine's convention says. A pass that called
//! `__gttf2` where it meant `__getf2` is one every test in that other layer passes.
//!
//! So this compiles `tests/quad/arithmetic.c` with the compiler this tree builds and with the system
//! compiler, runs both, and holds every group of cases against the other side. A difference names
//! the operation, which is what the groups are for, and the seed in the fixture is fixed, so the
//! case it happened at can be found again.
//!
//! `xtask/src/sides.rs` is the rest of it: which optimization levels the fixture is built at, why the
//! answers are the system compiler's rather than a table in this file, and the reading and comparing
//! of what every build printed. `xtask/src/wide.rs` is the other task of that shape.
//!
//! # What the undefined symbols are for
//!
//! A comparison at this format is the one place where agreeing on the answer is weak evidence. The
//! six ordered predicates come back from six routines whose answers differ only in sign, a program
//! that compares two ordinary numbers gets the same truth value out of four of the six, and the case
//! that tells them apart is the unordered one. So the fixture makes not a numbers on purpose, and the
//! object is also read with `nm -u`: every routine in the list below has to be one the build goes
//! looking for, which is how a lowering that quietly stopped being a call is told from one that
//! agreed.

use crate::runner::Runner;
use crate::sides;
use crate::{Result, root};

/// What this task is called, for the messages.
const TASK: &str = "quad";

/// The routines an operation at this format becomes a call to, which is every operation the format
/// has.
///
/// The arithmetic and the negation are five, the comparisons are seven, the conversions against the
/// two narrower floats are four, and the conversions against an integer are eight, a signed and an
/// unsigned one at thirty two bits and at sixty four in each direction. The fixture reaches all of
/// them, which for the seventh comparison takes `__builtin_isunordered`: no operator C has asks only
/// whether two values can be ordered, because each of the six has an answer for an unordered pair
/// built into which routine it calls.
const ROUTINES: [&str; 24] = [
    "__addtf3",
    "__subtf3",
    "__multf3",
    "__divtf3",
    "__negtf2",
    "__eqtf2",
    "__netf2",
    "__lttf2",
    "__letf2",
    "__gttf2",
    "__getf2",
    "__unordtf2",
    "__extendsftf2",
    "__extenddftf2",
    "__trunctfsf2",
    "__trunctfdf2",
    "__floatsitf",
    "__floatunsitf",
    "__floatditf",
    "__floatunditf",
    "__fixtfsi",
    "__fixunstfsi",
    "__fixtfdi",
    "__fixunstfdi",
];

/// Builds the fixture every way, runs all four programs, and compares what they printed.
///
/// # Errors
///
/// [`crate::Error::Io`] when the compiler or the archive will not build or the script will not run,
/// and [`crate::Error::Failed`] when a build of the fixture does not call the runtime routines, when
/// a program did not reach the end, or when any group of cases came out differently from the system
/// compiler's.
pub(crate) fn quad() -> Result<()> {
    let fixture = root().join("tests").join("quad").join("arithmetic.c");
    let work = sides::build(TASK, &fixture, SCRIPT)?;
    let runner = Runner::find("the binary128 arithmetic differential")?;
    let printed = runner.run(&work, "the binary128 arithmetic differential")?;
    sides::compare(TASK, &ROUTINES, &printed, &runner.to_string())
}

/// What the runner runs.
///
/// The archive goes on the link line ahead of everything the driver adds, so every call resolves to
/// the routine rucc compiled out of `runtime/builtins` rather than to libgcc's, which gcc puts at the
/// end of every link line it builds.
///
/// The reference is built at `-O1`. What level it is does not matter to the answers, since the
/// answers are C's, and a level nobody optimizes is a program whose loops are not the loops a program
/// would have.
const SCRIPT: &str = "\
#!/bin/sh
out=/tmp/quad
mkdir -p \"$out\"
gcc -O1 -o \"$out/reference\" arithmetic.c || exit 1
for level in 0 1 2; do
    gcc -o \"$out/ours-O$level\" \"ours-O$level.o\" librucc_builtins.a || exit 1
    nm -u \"ours-O$level.o\" | awk -v side=\"ours-O$level\" \\
        '$NF ~ /^__(add|sub|mul|div|neg|eq|ne|lt|le|gt|ge|unord)tf[23]$/ \\
            || $NF ~ /^__(extend[sd]ftf2|trunctf[sd]f2)$/ \\
            || $NF ~ /^__fix(uns)?tf(si|di)$/ \\
            || $NF ~ /^__float(un)?[sd]itf$/ { print side \" calls \" $NF }'
done
\"$out/reference\" | sed 's/^/reference /'
for level in 0 1 2; do
    \"$out/ours-O$level\" | sed \"s/^/ours-O$level /\"
done
";

#[cfg(test)]
mod tests {
    use super::*;

    /// Every routine the fixture is expected to reach is one the archive defines.
    ///
    /// A name in the list that nothing defines would be a check that fails at the link rather than at
    /// the comparison, and a name spelled wrong in the list is a check that fails for no reason at
    /// all. The routines are in `runtime/builtins/quad.c` and this reads it.
    #[test]
    fn every_routine_in_the_list_is_one_the_runtime_defines() {
        let text = std::fs::read_to_string(root().join("runtime").join("builtins").join("quad.c"))
            .expect("the runtime source is in the tree");
        for routine in ROUTINES {
            assert!(text.contains(routine), "{routine} is in the list and not in the runtime");
        }
    }
}
