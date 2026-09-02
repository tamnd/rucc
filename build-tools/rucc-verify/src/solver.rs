//! Running the solver.
//!
//! The solver is a program found on PATH, not a crate. A bitvector solver taken as a dependency
//! would be the largest thing in the tree by a wide margin, it would have to hold the 1.85
//! minimum the workspace holds, and `spec/18-package-layout.md` section 18.3 asks for a reason
//! before anything is added at all. Shelling out costs a process per rule, which is nothing
//! against the solving, and it means the version in use is the version CI installed and can say.

use std::io::Write;
use std::process::{Command, Stdio};

/// A solver that was found.
#[derive(Debug, Clone)]
pub struct Solver {
    program: String,
    seconds: u32,
}

/// What the solver said about one query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// No model exists, which is the answer a discharged rule gets: nothing makes the claim
    /// false.
    Unsat,
    /// A model exists, and here is what the solver printed of it. The rule is wrong.
    Sat(String),
    /// The solver gave up, usually on time. Not a failure of the rule and not a pass either.
    Unknown,
}

impl Solver {
    /// Look for a solver on PATH.
    ///
    /// Returns nothing when there is none, which is what lets the tests skip rather than fail on
    /// a machine that has not got one. CI has one, and that is where the answer matters.
    #[must_use]
    pub fn find() -> Option<Solver> {
        for program in ["z3", "cvc5"] {
            let found = Command::new(program).arg("--version").output();
            if found.is_ok_and(|out| out.status.success()) {
                return Some(Solver { program: program.to_owned(), seconds: 10 });
            }
        }
        None
    }

    /// How long a single query may take before the answer is [`Answer::Unknown`].
    #[must_use]
    pub fn within(self, seconds: u32) -> Solver {
        Solver { seconds, ..self }
    }

    /// What the solver is called, for a report that has to name it.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.program
    }

    /// Ask one question.
    ///
    /// # Errors
    ///
    /// Anything that stops the solver from running or from being talked to.
    pub fn ask(&self, query: &str) -> std::io::Result<Answer> {
        let timeout = match self.program.as_str() {
            "cvc5" => format!("--tlimit={}", self.seconds * 1000),
            _ => format!("-T:{}", self.seconds),
        };
        let stdin = if self.program == "cvc5" { "-" } else { "-in" };

        let mut child = Command::new(&self.program)
            .arg(stdin)
            .arg(timeout)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let Some(mut pipe) = child.stdin.take() else {
            return Err(std::io::Error::other("the solver has no standard input"));
        };
        pipe.write_all(query.as_bytes())?;
        drop(pipe);
        let out = child.wait_with_output()?;
        let said = String::from_utf8_lossy(&out.stdout);

        // The first line is the verdict and anything after it is the model, which is only asked
        // for when the verdict is `sat` and is the whole value of a refutation.
        let mut lines = said.lines();
        Ok(match lines.next().map(str::trim) {
            Some("unsat") => Answer::Unsat,
            Some("sat") => Answer::Sat(lines.collect::<Vec<_>>().join("\n")),
            _ => Answer::Unknown,
        })
    }
}
