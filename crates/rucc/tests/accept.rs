//! The accept and reject suite: programs that must compile under a dialect, and must not
//! compile under another.
//!
//! Design: `spec/15-testing.md` section 15.1, and the `M2` exit criterion in
//! `spec/17-milestones.md`.
//!
//! The cases are in `tests/accept` at the top of the repository. Each one is a `.c` file whose
//! leading comments say what is supposed to happen to it:
//!
//! ```text
//! /* accept: c99 c11 c17 c23 gnu */
//! /* reject: c89 */
//! /* message: unknown type name */
//! /* gap: #99 c89 */
//! ```
//!
//! One directive to a comment, and the old kind of comment, because a case that runs under
//! `-std=c89` cannot use `//` for its own directives.
//!
//! `accept` and `reject` take a list of dialects, and between them they have to name every one:
//! a dialect nobody mentioned is a dialect nobody thought about. `all`, `iso` and `gnu` stand
//! for the obvious groups. `message` is a substring every rejection has to contain, so that a
//! program rejected for the wrong reason is not counted as a pass. `warns` is the same thing for
//! an acceptance, and without it an acceptance has to be silent.
//!
//! `gap` names the dialects where the compiler does not do this yet, with the issue that says
//! when it will. Those are run backwards: the case is expected to fail, and the suite fails if
//! it starts passing, which is what closes the issue. That is the rule from `tests/README.md`,
//! which is that no test is deleted to make CI green.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Every dialect a case is run under.
const DIALECTS: [&str; 10] =
    ["c89", "c99", "c11", "c17", "c23", "gnu89", "gnu99", "gnu11", "gnu17", "gnu23"];

/// How many case and dialect pairs are known not to work yet.
///
/// This number is here rather than in a report nobody reads, so that adding a gap is a line in
/// a diff somebody has to approve. It is allowed to go down. Going up means the suite grew a
/// case for something that does not work yet, which is fine, and it means saying so out loud,
/// which is the point.
const KNOWN_GAPS: usize = 14;

/// Where the cases live.
fn accept_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate is two levels under the repository root")
        .join("tests")
        .join("accept")
}

/// What a case says should happen to it.
struct Expected {
    /// The dialects it has to compile under.
    accept: Vec<String>,
    /// The dialects it must not compile under.
    reject: Vec<String>,
    /// A substring every rejection has to contain.
    message: Option<String>,
    /// A substring every acceptance has to contain, with `None` meaning it has to be silent.
    warns: Option<String>,
    /// The dialects where the compiler is known to do the other thing, and why.
    gaps: Vec<(String, String)>,
}

/// The dialects one directive names, with the group names expanded.
fn dialects_in(list: &str, path: &Path) -> Vec<String> {
    let mut named = Vec::new();
    for word in list.split_whitespace() {
        let group: &[&str] = match word {
            "all" => &DIALECTS,
            "iso" => &["c89", "c99", "c11", "c17", "c23"],
            "gnu" => &["gnu89", "gnu99", "gnu11", "gnu17", "gnu23"],
            one if DIALECTS.contains(&one) => &[],
            other => panic!("{}: `{other}` is not a dialect", path.display()),
        };
        if group.is_empty() {
            named.push(word.to_owned());
        } else {
            named.extend(group.iter().map(|&d| d.to_owned()));
        }
    }
    named
}

/// The directives at the top of a case, checked for the mistakes that would make it test less
/// than it looks like it tests.
fn expectations(path: &Path, source: &str) -> Expected {
    let mut expected = Expected {
        accept: Vec::new(),
        reject: Vec::new(),
        message: None,
        warns: None,
        gaps: Vec::new(),
    };
    for line in source.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("/*").and_then(|rest| rest.strip_suffix("*/")) else {
            continue;
        };
        let Some((keyword, value)) = rest.split_once(':') else { continue };
        let value = value.trim();
        match keyword.trim() {
            "accept" => expected.accept.extend(dialects_in(value, path)),
            "reject" => expected.reject.extend(dialects_in(value, path)),
            "message" => expected.message = Some(value.to_owned()),
            "warns" => expected.warns = Some(value.to_owned()),
            "gap" => {
                let (issue, list) = value.split_once(char::is_whitespace).unwrap_or_else(|| {
                    panic!("{}: a gap needs an issue and a dialect", path.display())
                });
                for dialect in dialects_in(list, path) {
                    expected.gaps.push((dialect, issue.to_owned()));
                }
            }
            _ => {}
        }
    }
    for dialect in DIALECTS {
        let accepted = expected.accept.iter().any(|d| d == dialect);
        let rejected = expected.reject.iter().any(|d| d == dialect);
        assert!(
            accepted != rejected,
            "{}: {dialect} is {}, and every dialect has to be one or the other",
            path.display(),
            if accepted { "both accepted and rejected" } else { "neither accepted nor rejected" }
        );
    }
    expected
}

/// What the compiler did with one case under one dialect.
struct Ran {
    accepted: bool,
    said: String,
}

fn run(source: &Path, dialect: &str) -> Ran {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "--emit=tast", "-o", "-"])
        .arg(format!("-std={dialect}"))
        .arg(source)
        .output()
        .expect("the compiler is built before its own tests run");
    Ran { accepted: out.status.success(), said: String::from_utf8_lossy(&out.stderr).into_owned() }
}

#[test]
fn every_case_is_taken_the_way_its_directives_say_under_every_dialect() {
    let dir = accept_dir();
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.expect("a readable directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "c"))
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "{}: no cases", dir.display());

    let mut wrong = Vec::new();
    let mut closed = Vec::new();
    let mut gaps = 0;
    for case in &cases {
        let name = case.file_name().unwrap_or(case.as_os_str()).to_string_lossy().into_owned();
        let source = std::fs::read_to_string(case).unwrap_or_else(|e| panic!("{name}: {e}"));
        let expected = expectations(case, &source);
        for dialect in DIALECTS {
            let should_accept = expected.accept.iter().any(|d| d == dialect);
            let gap = expected.gaps.iter().find(|(d, _)| d == dialect);
            let ran = run(case, dialect);
            if let Some((_, issue)) = gap {
                gaps += 1;
                if ran.accepted == should_accept {
                    closed.push(format!(
                        "{name} at -std={dialect}: works now, so the gap on {issue} can go"
                    ));
                }
                continue;
            }
            if ran.accepted != should_accept {
                let happened = if ran.accepted { "was accepted" } else { "was rejected" };
                wrong.push(format!(
                    "{name} at -std={dialect}: {happened} and should not have been. {}",
                    ran.said.trim().replace('\n', "; ")
                ));
                continue;
            }
            if should_accept {
                match &expected.warns {
                    Some(wanted) => assert!(
                        ran.said.contains(wanted),
                        "{name} at -std={dialect}: expected a warning saying `{wanted}`, said `{}`",
                        ran.said.trim()
                    ),
                    None => assert!(
                        ran.said.is_empty(),
                        "{name} at -std={dialect}: compiled but said `{}`, and an acceptance with \
                         no `warns` directive has to be silent",
                        ran.said.trim()
                    ),
                }
            } else if let Some(wanted) = &expected.message {
                assert!(
                    ran.said.contains(wanted),
                    "{name} at -std={dialect}: rejected for the wrong reason. Expected \
                     `{wanted}`, said `{}`",
                    ran.said.trim()
                );
            }
        }
    }
    assert!(closed.is_empty(), "{}", closed.join("\n"));
    assert!(wrong.is_empty(), "{} wrong answer(s):\n{}", wrong.len(), wrong.join("\n"));
    assert_eq!(
        gaps, KNOWN_GAPS,
        "the number of known gaps changed. Update KNOWN_GAPS, and say in the pull request which \
         way it went and why."
    );
}
