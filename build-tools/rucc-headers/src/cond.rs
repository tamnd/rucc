//! The conditionals the merge writes, and reading them back.
//!
//! Design: `spec/cross-compile/08-sysroots.md` section 8.3. The macro is `__GLIBC_MINOR__` because
//! that is glibc's own spelling and the one `__GLIBC_PREREQ` reads, so every autoconf probe ever
//! written agrees with it. The tree does not define it; the compiler does, from the tuple's
//! `env_version`, which is `rucc_sysroot::bundled_glibc_minor`.
//!
//! # Why the oldest branch has no lower bound
//!
//! Because a release older than the oldest one surveyed is served by defining the macro lower, and
//! a floor of `__GLIBC_MINOR__ >= 28` on the oldest branch would serve it nothing at all. A tree
//! that hands 2.17 the declarations of 2.28 is too permissive, which is the direction section 8.3
//! accepts and document 09 section 9.5 describes; a tree that hands it an empty file is broken. So
//! the branches cover every value of the macro, the oldest surveyed release's text is what anything
//! below it gets, and the one place that says a version was never surveyed is the guard that
//! replaces glibc's own definition of the macro.
//!
//! # Why every directive carries a marker
//!
//! Because the merge has to be able to read its own output back, to check that what it wrote
//! reproduces each release, and a header full of real `#if` and `#endif` lines gives it no way to
//! tell which ones are its own. Counting nesting is not enough: the files being merged are the ones
//! with eight levels of conditionals in them. So every directive this module writes ends in
//! [`MARK`], nothing else in the tree is allowed to contain that text, and reading the output back
//! is then a matter of looking at the end of the line.

/// What every directive the merge writes ends with.
///
/// A comment, so it changes nothing about what the preprocessor does, and glibc's own headers end
/// their `#endif` lines with comments too, so it reads like the text around it.
pub const MARK: &str = "/* rucc */";

/// The macro the conditionals are written against.
pub const MACRO: &str = "__GLIBC_MINOR__";

/// The releases a tree is merged from, in ascending order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Releases {
    minors: Vec<u32>,
}

impl Releases {
    /// The releases, which have to be ascending, distinct and at least two.
    ///
    /// At least two because merging one release is copying it, and a tool that accepts that
    /// silently is a tool somebody will run by mistake and ship the result of.
    pub fn new(minors: Vec<u32>) -> Result<Self, String> {
        if minors.len() < 2 {
            return Err("merging wants at least two releases, and one is a copy".to_owned());
        }
        for pair in minors.windows(2) {
            if pair[0] >= pair[1] {
                return Err(format!(
                    "the releases have to be ascending and distinct, and 2.{} comes after 2.{}",
                    pair[0], pair[1]
                ));
            }
        }
        Ok(Self { minors })
    }

    /// How many releases there are. Never none, and never one.
    pub fn count(&self) -> usize {
        self.minors.len()
    }

    /// The releases themselves.
    pub fn minors(&self) -> &[u32] {
        &self.minors
    }

    /// How the release at `at` is spelled in a message.
    pub fn spelled(&self, at: usize) -> String {
        format!("2.{}", self.minors[at])
    }

    /// The condition that holds for exactly the releases marked in `present`, or `None` when that
    /// is all of them and no conditional is wanted.
    ///
    /// A release between two surveyed ones belongs to the older of the two, because that is what
    /// the bounds say and because the alternative is claiming to know what a release we did not
    /// look at contains.
    ///
    /// # Panics
    ///
    /// When `present` is not one flag per release, which is a mistake in the caller.
    pub fn condition(&self, present: &[bool]) -> Option<String> {
        assert_eq!(present.len(), self.minors.len(), "a presence set has one flag per release");
        if present.iter().all(|&p| p) {
            return None;
        }
        // One run of consecutive releases at a time, each becoming a term of its own.
        let mut runs: Vec<Vec<String>> = Vec::new();
        let mut at = 0;
        while at < present.len() {
            if !present[at] {
                at += 1;
                continue;
            }
            let start = at;
            while at + 1 < present.len() && present[at + 1] {
                at += 1;
            }
            let mut atoms: Vec<String> = Vec::new();
            // No lower bound on a run that starts at the oldest release, for the reason in the
            // module documentation.
            if start > 0 {
                atoms.push(format!("{MACRO} >= {}", self.minors[start]));
            }
            if at + 1 < present.len() {
                atoms.push(format!("{MACRO} < {}", self.minors[at + 1]));
            }
            runs.push(atoms);
            at += 1;
        }
        if runs.is_empty() {
            // Nothing is present, which the merge never asks for, and `0` is the honest answer.
            return Some("0".to_owned());
        }
        // Parentheses only where there is something to group, because a condition a reviewer has
        // to count brackets in is a condition a reviewer skips.
        let one = runs.len() == 1;
        let terms: Vec<String> = runs
            .into_iter()
            .map(|atoms| match atoms.len() {
                0 => "1".to_owned(),
                1 => atoms.into_iter().next().unwrap_or_default(),
                _ if one => atoms.join(" && "),
                _ => format!("({})", atoms.join(" && ")),
            })
            .collect();
        Some(terms.join(" || "))
    }
}

/// One directive line, marked as ours, with its newline on it.
pub fn directive(keyword: &str, condition: Option<&str>) -> String {
    match condition {
        Some(text) => format!("#{keyword} {text} {MARK}\n"),
        None => format!("#{keyword} {MARK}\n"),
    }
}

/// Whether a release's own text would be mistaken for the merge's own directives.
pub fn carries_mark(text: &str) -> bool {
    text.contains(MARK)
}

/// What the preprocessor would leave of a merged file, for one value of the macro.
///
/// This is how the merge checks itself: the text it is about to write, read back the way a compiler
/// would read it, has to be the release it came from. Only the merge's own directives are
/// evaluated. Everything else is text, including the header's own conditionals, because what this
/// has to reproduce is the file as a release shipped it and not what a compilation of it means.
pub fn evaluate(text: &str, minor: u32) -> Result<String, String> {
    struct Frame {
        outer: bool,
        taken: bool,
        active: bool,
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut out = String::with_capacity(text.len());
    for (n, line) in text.split_inclusive('\n').enumerate() {
        let at = n + 1;
        let active = stack.last().is_none_or(|f| f.active);
        let Some(body) = ours(line) else {
            if active {
                out.push_str(line);
            }
            continue;
        };
        if let Some(condition) = body.strip_prefix("#if ") {
            let holds = holds(condition.trim(), minor).map_err(|why| format!("{at}: {why}"))?;
            stack.push(Frame { outer: active, taken: holds, active: active && holds });
        } else if let Some(condition) = body.strip_prefix("#elif ") {
            let holds = holds(condition.trim(), minor).map_err(|why| format!("{at}: {why}"))?;
            let frame = stack.last_mut().ok_or(format!("{at}: #elif with no #if"))?;
            frame.active = frame.outer && !frame.taken && holds;
            frame.taken = frame.taken || holds;
        } else if body == "#else" {
            let frame = stack.last_mut().ok_or(format!("{at}: #else with no #if"))?;
            frame.active = frame.outer && !frame.taken;
            frame.taken = true;
        } else if body == "#endif" {
            stack.pop().ok_or(format!("{at}: #endif with no #if"))?;
        } else {
            return Err(format!("{at}: {body} is marked as ours and is not a directive"));
        }
    }
    if stack.is_empty() {
        Ok(out)
    } else {
        Err(format!("{} of our conditionals are still open at the end", stack.len()))
    }
}

/// The directive on this line, if the line is one of ours.
fn ours(line: &str) -> Option<&str> {
    let body = line.trim_end();
    let body = body.strip_suffix(MARK)?.trim_end();
    body.starts_with('#').then_some(body)
}

/// Whether one of our conditions holds, which is the only grammar this has to read.
fn holds(condition: &str, minor: u32) -> Result<bool, String> {
    let mut any = false;
    for term in condition.split("||") {
        let term = term.trim();
        let term = match term.strip_prefix('(') {
            Some(rest) => {
                rest.strip_suffix(')').ok_or(format!("unbalanced parentheses: {term}"))?
            }
            None => term,
        };
        let mut all = true;
        for atom in term.split("&&") {
            all &= atom_holds(atom.trim(), minor)?;
        }
        any |= all;
    }
    Ok(any)
}

/// Whether one comparison holds. `1` and `0` are the two conditions that name no version.
fn atom_holds(atom: &str, minor: u32) -> Result<bool, String> {
    match atom {
        "1" => return Ok(true),
        "0" => return Ok(false),
        _ => {}
    }
    let rest = atom.strip_prefix(MACRO).ok_or(format!("not a condition of ours: {atom}"))?;
    let rest = rest.trim_start();
    if let Some(value) = rest.strip_prefix(">=") {
        Ok(minor >= number(value)?)
    } else if let Some(value) = rest.strip_prefix('<') {
        Ok(minor < number(value)?)
    } else {
        Err(format!("not a comparison of ours: {atom}"))
    }
}

/// The version on the right of a comparison.
fn number(text: &str) -> Result<u32, String> {
    text.trim().parse().map_err(|_| format!("not a version: {text}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EIGHT: [u32; 8] = [28, 31, 34, 35, 36, 39, 41, 44];

    fn releases() -> Releases {
        Releases::new(EIGHT.to_vec()).expect("ascending and distinct")
    }

    fn present(of: &[u32]) -> Vec<bool> {
        EIGHT.iter().map(|m| of.contains(m)).collect()
    }

    /// What a condition means is which releases it holds for, so that is what the tests check,
    /// rather than the text, with one exception below that is about the text.
    fn holding(condition: &Option<String>) -> Vec<u32> {
        EIGHT
            .iter()
            .copied()
            .filter(|&m| match condition {
                None => true,
                Some(text) => holds(text, m).expect("our own grammar"),
            })
            .collect()
    }

    #[test]
    fn every_subset_of_eight_releases_gets_a_condition_that_means_it() {
        let all = releases();
        for bits in 0u32..256 {
            let chosen: Vec<u32> = EIGHT
                .iter()
                .enumerate()
                .filter(|(n, _)| bits & (1 << n) != 0)
                .map(|(_, &m)| m)
                .collect();
            let condition = all.condition(&present(&chosen));
            assert_eq!(holding(&condition), chosen, "{condition:?}");
        }
    }

    #[test]
    fn the_whole_set_wants_no_conditional() {
        assert_eq!(releases().condition(&[true; 8]), None);
    }

    #[test]
    fn the_shapes_a_reviewer_reads() {
        let all = releases();
        assert_eq!(all.condition(&present(&[44])), Some("__GLIBC_MINOR__ >= 44".to_owned()));
        assert_eq!(
            all.condition(&present(&[39, 41, 44])),
            Some("__GLIBC_MINOR__ >= 39".to_owned())
        );
        assert_eq!(all.condition(&present(&[28])), Some("__GLIBC_MINOR__ < 31".to_owned()));
        assert_eq!(all.condition(&present(&[28, 31])), Some("__GLIBC_MINOR__ < 34".to_owned()));
        assert_eq!(
            all.condition(&present(&[34, 35])),
            Some("__GLIBC_MINOR__ >= 34 && __GLIBC_MINOR__ < 36".to_owned())
        );
        assert_eq!(
            all.condition(&present(&[28, 41, 44])),
            Some("__GLIBC_MINOR__ < 31 || __GLIBC_MINOR__ >= 41".to_owned())
        );
        assert_eq!(
            all.condition(&present(&[31, 34, 44])),
            Some(
                "(__GLIBC_MINOR__ >= 31 && __GLIBC_MINOR__ < 35) || __GLIBC_MINOR__ >= 44"
                    .to_owned()
            )
        );
    }

    /// A release nobody surveyed belongs to the newest surveyed release that is not newer than it,
    /// and anything older than the oldest one surveyed gets that one.
    #[test]
    fn a_release_between_two_surveyed_ones_belongs_to_the_older() {
        let all = releases();
        let condition = all.condition(&present(&[28, 31])).expect("not everything");
        for minor in [0, 17, 28, 30, 31, 33] {
            assert!(holds(&condition, minor).expect("ours"), "2.{minor}");
        }
        for minor in [34, 35, 44, 99] {
            assert!(!holds(&condition, minor).expect("ours"), "2.{minor}");
        }
    }

    #[test]
    fn a_run_of_two_releases_and_nothing_else_is_rejected_as_a_set_of_one() {
        assert!(Releases::new(vec![28]).is_err());
        assert!(Releases::new(vec![31, 28]).is_err());
        assert!(Releases::new(vec![28, 28]).is_err());
        assert!(Releases::new(vec![28, 31]).is_ok());
    }

    #[test]
    fn a_conditional_is_read_back_the_way_it_was_written() {
        let text = format!(
            "common\n{}new\n{}old\n{}tail\n",
            directive("if", Some("__GLIBC_MINOR__ >= 34")),
            directive("else", None),
            directive("endif", None),
        );
        assert_eq!(evaluate(&text, 34).expect("ours"), "common\nnew\ntail\n");
        assert_eq!(evaluate(&text, 31).expect("ours"), "common\nold\ntail\n");
    }

    #[test]
    fn an_elif_chain_takes_the_first_branch_that_holds_and_no_other() {
        let text = format!(
            "{}a\n{}b\n{}c\n{}",
            directive("if", Some("__GLIBC_MINOR__ >= 41")),
            directive("elif", Some("__GLIBC_MINOR__ >= 34")),
            directive("else", None),
            directive("endif", None),
        );
        assert_eq!(evaluate(&text, 44).expect("ours"), "a\n");
        assert_eq!(evaluate(&text, 36).expect("ours"), "b\n");
        assert_eq!(evaluate(&text, 28).expect("ours"), "c\n");
    }

    /// The header's own conditionals are text, and the marker is what keeps them apart from ours.
    #[test]
    fn the_files_own_conditionals_are_left_alone() {
        let text = format!(
            "#ifdef __USE_GNU\n{}int f (void);\n{}#endif\n",
            directive("if", Some("__GLIBC_MINOR__ >= 34")),
            directive("endif", None),
        );
        assert_eq!(evaluate(&text, 44).expect("ours"), "#ifdef __USE_GNU\nint f (void);\n#endif\n");
        assert_eq!(evaluate(&text, 28).expect("ours"), "#ifdef __USE_GNU\n#endif\n");
    }

    #[test]
    fn a_branch_inside_a_branch_that_is_not_taken_stays_shut() {
        let text = format!(
            "{}outer\n{}inner\n{}{}",
            directive("if", Some("__GLIBC_MINOR__ >= 41")),
            directive("if", Some("__GLIBC_MINOR__ >= 44")),
            directive("endif", None),
            directive("endif", None),
        );
        assert_eq!(evaluate(&text, 44).expect("ours"), "outer\ninner\n");
        assert_eq!(evaluate(&text, 41).expect("ours"), "outer\n");
        assert_eq!(evaluate(&text, 28).expect("ours"), "");
    }

    #[test]
    fn an_unfinished_conditional_of_ours_is_an_error_and_not_a_guess() {
        let text = directive("if", Some("__GLIBC_MINOR__ >= 34"));
        assert!(evaluate(&text, 34).is_err());
        assert!(evaluate(&directive("endif", None), 34).is_err());
        assert!(evaluate(&directive("else", None), 34).is_err());
    }

    #[test]
    fn a_condition_we_did_not_write_is_an_error() {
        let text = format!("#if defined __USE_GNU {MARK}\n{}", directive("endif", None));
        let why = evaluate(&text, 34).expect_err("not our grammar");
        assert!(why.contains("not a condition of ours"), "{why}");
    }

    #[test]
    fn the_marker_is_what_a_tree_is_refused_for_carrying() {
        assert!(carries_mark(&directive("endif", None)));
        assert!(!carries_mark("#endif /* features.h */\n"));
    }
}
