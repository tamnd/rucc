//! Arithmetic on a 128-bit integer, held against the system compiler by running it.
//!
//! Design: `spec/12-abi-and-runtime.md` section 12.8, which is where the routines a division calls
//! are, and where what runs them is.
//!
//! `crates/rucc-codegen/src/wide.rs` is the pass that makes this width work: every value becomes the
//! two registers the convention holds it in and every operation over one becomes operations over the
//! halves, except the four divisions and the conversions to and from a float, which become a call to
//! the routines `runtime/builtins/div.c` and `runtime/builtins/convert.c` define. What checked that
//! until now was the pass's own tests, which build a function, run the pass and read the IR back.
//! That is the same kind of evidence as a stub writer reading its own bytes back: it catches a pass
//! that did something other than what it meant to and not a pass that meant the wrong thing. Nothing
//! ran the code.
//!
//! So this compiles `tests/wide/arithmetic.c` with the compiler this tree builds and with the system
//! compiler, runs both, and holds every group of cases against the other side. A difference names
//! the operation, which is what the groups are for, and the seed in the fixture is fixed, so the
//! case it happened at can be found again.
//!
//! The overflow checking builtins are in the fixture too, and they are the one thing in it that is
//! not that pass. `__builtin_add_overflow` and its two neighbours are rewritten into ordinary
//! arithmetic before the splitting runs, so the pass sees adds, multiplies and comparisons it
//! already knows, and what the fixture is asking is whether the rewriting picked the right ones. It
//! is here rather than somewhere of its own because the awkward case is a hundred and twenty eight
//! bit unsigned operand beside a signed one, which needs a hundred and twenty nine bits to hold both
//! and so is done by carrying each operand's sign alongside its value. That is tamnd/rucc#602, and
//! it is a shape only this width has.
//!
//! `xtask/src/sides.rs` is the rest of it: which optimization levels the fixture is built at, why
//! the answers are the system compiler's rather than a table in this file, and the reading and
//! comparing of what every build printed. The second task of this shape is what moved them there.

use crate::runner::Runner;
use crate::sides;
use crate::{Result, root};

/// What this task is called, for the messages.
const TASK: &str = "wide";

/// The routines an operation at this width becomes a call to, which are the four divisions and the
/// eight conversions against a `float` and a `double`.
const ROUTINES: [&str; 12] = [
    "__udivti3",
    "__umodti3",
    "__divti3",
    "__modti3",
    "__floattidf",
    "__floattisf",
    "__floatuntidf",
    "__floatuntisf",
    "__fixdfti",
    "__fixsfti",
    "__fixunsdfti",
    "__fixunssfti",
];

/// Builds the fixture every way, runs all four programs, and compares what they printed.
///
/// # Errors
///
/// [`Error::Io`] when the compiler or the archive will not build or the script will not run, and
/// [`Error::Failed`] when a build of the fixture does not call the runtime routines, when a program
/// did not reach the end, or when any group of cases came out differently from the system
/// compiler's.
pub(crate) fn wide() -> Result<()> {
    let fixture = root().join("tests").join("wide").join("arithmetic.c");
    let work = sides::build(TASK, &fixture, SCRIPT)?;
    let runner = Runner::find("the wide arithmetic differential")?;
    let printed = runner.run(&work, "the wide arithmetic differential")?;
    sides::compare(TASK, &ROUTINES, &printed, &runner.to_string())
}

/// What the runner runs.
///
/// The archive goes on the link line ahead of everything the driver adds, so a division or a
/// conversion resolves to the routine rucc compiled out of `runtime/builtins` rather than to
/// libgcc's, which gcc puts at the end of every link line it builds. Which one answered is also read
/// back: the object's undefined symbols say whether the operations in the fixture became calls at
/// all, and a build where they did not is a check that would pass for the wrong reason.
///
/// The reference is built at `-O1`. What level it is does not matter to the answers, since the
/// answers are C's, and a level nobody optimizes is a program whose loops are not the loops a program
/// would have.
const SCRIPT: &str = "\
#!/bin/sh
out=/tmp/wide
mkdir -p \"$out\"
gcc -O1 -o \"$out/reference\" arithmetic.c || exit 1
for level in 0 1 2; do
    gcc -o \"$out/ours-O$level\" \"ours-O$level.o\" librucc_builtins.a || exit 1
    nm -u \"ours-O$level.o\" | awk -v side=\"ours-O$level\" \\
        '/__(u?div|u?mod)ti3$/ || /__float(un)?ti[sd]f$/ || /__fix(uns)?[sd]fti$/ \\
            { print side \" calls \" $NF }'
done
\"$out/reference\" | sed 's/^/reference /'
for level in 0 1 2; do
    \"$out/ours-O$level\" | sed \"s/^/ours-O$level /\"
done
";
