//! Running the solver.
//!
//! The solver is a program found on PATH, not a crate. A bitvector solver taken as a dependency
//! would be the largest thing in the tree by a wide margin, it would have to hold the 1.85
//! minimum the workspace holds, and `spec/18-package-layout.md` section 18.3 asks for a reason
//! before anything is added at all. Shelling out costs a process per rule, which is nothing
//! against the solving, and it means the version in use is the version CI installed and can say.

use std::io::Write;
use std::process::{Command, Stdio};

/// How long one query gets before the answer is [`Answer::Unknown`], in seconds.
///
/// Five minutes, and the number is measured rather than picked. Of the 571 rules the gate is
/// given, all but five are settled in well under a second each, and whole files of them come back
/// in under a second together. Four of the five are in `crates/rucc-opt/rules/safety.rules` and
/// ask about a walk over an object at sixty four bits: five seconds each for `swept` and
/// `swept.sym`, twenty for `reached`, and fifty four for `swept.down.sym`, which is the largest
/// claim in the tree. That is z3 5.1.0 on a laptop with nothing else running. The fifth is the
/// multiply against division in `crates/rucc-codegen/rules/x86-64.rules`, which no budget settles
/// and which carries a written reason for the bounded proof it gets instead.
///
/// The same solver on a six core Linux box, which is the class of machine CI runs on, costs
/// twenty four seconds for `reached` and between seventy five and eighty three for the downward
/// sweep. Eighty three against the ninety this used to be is not a budget, it is a race the
/// slower machine sometimes loses, and losing it reads as a rule nobody has proved. That is how
/// the same tree proved and failed to prove minutes apart. tamnd/rucc#949.
///
/// The cost of a limit this loose is paid only by a rule that is genuinely not going to settle,
/// and that rule stops the build either way. The cost of one too tight is a rule that is fine
/// being reported as unproved, which reads as a real problem and is not one.
const DEFAULT: u32 = 300;

/// A solver that was found.
#[derive(Debug, Clone)]
pub struct Solver {
    program: String,
    seconds: u32,
}

/// What the verifier is allowed to want of a solver.
///
/// [`Solver`] is the implementation that is a solver, and it is the one the gate runs. The reason
/// there is a trait over it at all is the other kind: a test about what happens after the solver
/// gives up has no way to make the real one give up except by starving it of time, and a test
/// whose meaning depends on how fast the machine is says something slightly different everywhere
/// it runs. That is tamnd/rucc#1123. A stub that answers unknown to the one question no solver
/// settles says the thing the test is about.
///
/// Two methods, because two is what [`fn@crate::verify`] calls. This is a test double and not an
/// abstraction anybody else has to hold: nothing outside this crate implements it, and a second
/// real solver would be another [`Solver::find`] rather than another implementation of this.
pub trait Ask {
    /// Put one question, and allow it this many seconds.
    ///
    /// The budget is a parameter rather than a property of the solver because a rule that already
    /// carries a written reason is given a look rather than the whole of it, and that decision
    /// belongs to the caller who knows which rule it is.
    ///
    /// # Errors
    ///
    /// Anything that stops the solver from running or from being talked to.
    fn ask(&self, query: &str, seconds: u32) -> std::io::Result<Answer>;

    /// How long a question gets when nothing has narrowed it.
    ///
    /// A run that says what the budget was is a run whose shrug can be read. Without it, a rule
    /// reported as unproved is either a rule that is false or a rule that ran out of a number
    /// nobody printed, and telling those apart is the whole difficulty of tamnd/rucc#949.
    fn seconds(&self) -> u32;
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
                return Some(Solver { program: program.to_owned(), seconds: DEFAULT });
            }
        }
        None
    }

    /// How long a single query may take before the answer is [`Answer::Unknown`].
    ///
    /// This is the number [`Ask::seconds`] reports, so it is what a rule gets unless the caller
    /// narrows it for that rule.
    #[must_use]
    pub fn within(self, seconds: u32) -> Solver {
        Solver { seconds, ..self }
    }

    /// What the solver is called, for a report that has to name it.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.program
    }
}

impl Ask for Solver {
    fn seconds(&self) -> u32 {
        self.seconds
    }

    fn ask(&self, query: &str, seconds: u32) -> std::io::Result<Answer> {
        let timeout = match self.program.as_str() {
            "cvc5" => format!("--tlimit={}", seconds * 1000),
            _ => format!("-T:{seconds}"),
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
