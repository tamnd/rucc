//! Calls in tail position, held against the system compiler by running them.
//!
//! Design: `spec/optimizer/25-tail-calls.md` section 25.2, the sibling call.
//!
//! `crates/rucc-codegen/src/tail.rs` turns `return f(x)` into the epilogue and a jump to `f`. Its
//! tests say which calls it takes and which it turns down. What they do not say is that the jump
//! leaves every argument where the callee looks for it and the answer where the caller's caller
//! looks, which is a question about registers after allocation and so about code that ran. So this
//! compiles `tests/tail/calls.c` with the compiler this tree builds and with the system compiler,
//! runs both, and holds every group of calls against the other side. `xtask/src/sides.rs` is that
//! part.
//!
//! The other thing a tail call is for is the stack, and that is checked on its own. The fixture
//! run with an argument walks ten million calls between two functions, and the script runs it
//! under a one megabyte stack at `-O2`, where a chain of calls that each kept a frame would need a
//! hundred and sixty. A build that got through printed the same answer as gcc's, and one that did
//! not died on the stack and printed nothing.

use crate::runner::Runner;
use crate::sides;
use crate::{Error, Result, root};

/// What this task is called, for the messages.
const TASK: &str = "tail";

/// What the deep run is called in the output, in front of the side it ran on. Not a side
/// [`sides::read`] knows, so these lines are left to [`deep`].
const DEEP: &str = "deep";

/// Builds the fixture every way, runs every program, and compares what they printed.
///
/// # Errors
///
/// [`crate::Error::Io`] when the compiler will not build or the script will not run, and
/// [`crate::Error::Failed`] when a program did not reach the end, when any group of calls came
/// out differently from the system compiler's, or when the deep chain did not fit in the stack.
pub(crate) fn tail() -> Result<()> {
    let fixture = root().join("tests").join("tail").join("calls.c");
    let work = sides::build(TASK, &fixture, SCRIPT)?;
    let runner = Runner::find("the tail call differential")?;
    let printed = runner.run(&work, "the tail call differential")?;
    deep(&printed)?;
    sides::compare(TASK, &[], &printed, &runner.to_string())
}

/// Whether the deep chain at `-O2` got to the end in a small stack and came to gcc's answer.
fn deep(printed: &str) -> Result<()> {
    let said = |side: &str| {
        let prefix = format!("{DEEP} {side} deep ");
        printed
            .lines()
            .find_map(|line| line.strip_prefix(&prefix).map(str::trim).map(str::to_owned))
    };
    let theirs = said(sides::REFERENCE);
    let ours = said("ours-O2");
    let problem = match (&theirs, &ours) {
        (None, _) => format!(
            "the {} program did not get through the deep chain in a one megabyte stack, so there \
             is nothing to compare against",
            sides::REFERENCE
        ),
        (Some(_), None) => "ours-O2 did not get through the deep chain in a one megabyte stack, \
                            which is a call in tail position that kept its frame"
            .to_owned(),
        (Some(theirs), Some(ours)) if theirs != ours => format!(
            "ours-O2 came to {ours} at the end of the deep chain where {} came to {theirs}",
            sides::REFERENCE
        ),
        _ => return Ok(()),
    };
    Err(Error::Failed { task: TASK, problems: vec![problem] })
}

/// What the runner runs.
///
/// The reference is built at `-O2`, since that is where gcc turns sibling calls on. The deep chain
/// runs in a subshell so the smaller stack is only its own.
const SCRIPT: &str = "\
#!/bin/sh
out=/tmp/tail
mkdir -p \"$out\"
gcc -O2 -o \"$out/reference\" calls.c || exit 1
for level in 0 1 2; do
    gcc -o \"$out/ours-O$level\" \"ours-O$level.o\" || exit 1
done
\"$out/reference\" | sed 's/^/reference /'
for level in 0 1 2; do
    \"$out/ours-O$level\" | sed \"s/^/ours-O$level /\"
done
(ulimit -s 1024; \"$out/reference\" deep) | sed 's/^/deep reference /'
(ulimit -s 1024; \"$out/ours-O2\" deep) | sed 's/^/deep ours-O2 /'
exit 0
";

#[cfg(test)]
mod tests {
    use super::deep;

    #[test]
    fn a_deep_chain_that_came_to_the_same_answer_passes() {
        let printed = "deep reference deep 42\ndeep ours-O2 deep 42\n";
        assert!(deep(printed).is_ok());
    }

    #[test]
    fn a_deep_chain_that_ran_out_of_stack_fails() {
        let printed = "deep reference deep 42\n";
        assert!(deep(printed).is_err());
    }

    #[test]
    fn a_deep_chain_that_came_to_a_different_answer_fails() {
        let printed = "deep reference deep 42\ndeep ours-O2 deep 41\n";
        assert!(deep(printed).is_err());
    }
}
