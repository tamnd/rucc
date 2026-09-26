//! Division by a constant, held against the system compiler by running it.
//!
//! Design: `spec/optimizer/19-reassociation-and-arithmetic.md` section 19.5, which asks for the
//! rewrite to be checked where every compiler has had it wrong.
//!
//! `crates/rucc-codegen/src/divide.rs` turns a division or a remainder by a constant into a
//! multiply by a magic number and some shifts. Its tests run the plan it makes on numbers, which
//! says the arithmetic is right. What they do not say is that the plan went into the function the
//! way it was planned and that the selector made the instructions it meant, and a wrong shift width
//! there is a wrong answer for some dividends and not others. So this compiles
//! `tests/divide/constants.c` with the compiler this tree builds and with the system compiler, runs
//! both, and holds every group of divisions against the other side.
//!
//! No routine is expected in the objects. Every division here is either rewritten or left as the
//! `div` it was, and which of the two it was is for the pass's tests to say. What is asked here is
//! only whether the answers are C's. `xtask/src/sides.rs` is the rest of it.

use crate::runner::Runner;
use crate::sides;
use crate::{Result, root};

/// What this task is called, for the messages.
const TASK: &str = "divide";

/// Builds the fixture every way, runs all four programs, and compares what they printed.
///
/// # Errors
///
/// [`crate::Error::Io`] when the compiler will not build or the script will not run, and
/// [`crate::Error::Failed`] when a program did not reach the end or when any group of divisions
/// came out differently from the system compiler's.
pub(crate) fn divide() -> Result<()> {
    let fixture = root().join("tests").join("divide").join("constants.c");
    let work = sides::build(TASK, &fixture, SCRIPT)?;
    let runner = Runner::find("the division differential")?;
    let printed = runner.run(&work, "the division differential")?;
    sides::compare(TASK, &[], &printed, &runner.to_string())
}

/// What the runner runs.
///
/// The reference is built at `-O2`, since that is where gcc rewrites its own divisions and so where
/// the two compilers' rewrites are held against each other, as well as against C.
const SCRIPT: &str = "\
#!/bin/sh
out=/tmp/divide
mkdir -p \"$out\"
gcc -O2 -o \"$out/reference\" constants.c || exit 1
for level in 0 1 2; do
    gcc -o \"$out/ours-O$level\" \"ours-O$level.o\" || exit 1
done
\"$out/reference\" | sed 's/^/reference /'
for level in 0 1 2; do
    \"$out/ours-O$level\" | sed \"s/^/ours-O$level /\"
done
";
