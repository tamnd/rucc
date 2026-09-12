//! One header, merged across every release that has it.
//!
//! Design: `spec/cross-compile/08-sysroots.md` section 8.3. The shape of the answer is Zig's
//! `generic-glibc`, which is one tree with the per-version differences written inside the files as
//! conditionals on `__GLIBC_MINOR__`, and the technique for deriving one rather than maintaining it
//! by hand is `ziglang/universal-headers`, which document 01.2 records as the state of the art and
//! as unfinished.
//!
//! # What a merge is allowed to do
//!
//! Write text that every release shipped, and conditionals around the text that only some of them
//! shipped. That is all. It never edits a declaration, never reflows anything and never invents a
//! line, with one exception that is not an exception so much as a requirement: glibc's own
//! definition of `__GLIBC_MINOR__` has to go, because the compiler is what defines it in a merged
//! tree, and what replaces it is a check that somebody did.
//!
//! # The two ways cutting a file up can break it
//!
//! A conditional put in the wrong place turns a working header into one that does not compile, or
//! worse into one that compiles differently, so both ways are checked rather than avoided by being
//! careful.
//!
//! The first is a piece that cannot be split, which is a macro or a directive continued with a
//! backslash, or a line whose trailing comment closes further down. `norm::pieces` is what makes
//! that impossible: the smallest thing the merge can put a conditional between is a logical line.
//!
//! The second is a piece that is part of the file's own conditional. A region that contains an
//! `#endif` whose `#if` is above it, or an `#else` belonging to an `#if` above it, cannot be wrapped
//! in an `#if` of ours: our `#endif` would close theirs, or their `#else` would become ours.
//!
//! The answer to the second one is to make the region bigger until it holds the whole of whatever it
//! was reaching into, and the region says which way to grow rather than being searched for. A branch
//! that closes an `#if` from above needs the text above it, a branch that leaves an `#if` of its own
//! open needs the text below it, and growing stops as soon as no branch reaches out. Growing
//! upward takes back a region already decided, which is the one place a decision here is
//! reconsidered. A file where the region grows to the whole file is one copy per release, which is
//! how this started and is still the answer for a file whose releases disagree about where their own
//! conditionals are.
//!
//! # Why it reads its own output back
//!
//! Because the only statement worth making about a merged tree is that it is the releases it was
//! merged from, and the way to say that is to take it apart again. Every file, for every release
//! that has it, is evaluated at that release's version and held against what that release shipped.
//! A merge that cannot reproduce its inputs is a bug in this file, and the check runs before the
//! tree is written rather than in a test over a fixture, because the fixture that matters is
//! glibc.

use crate::cond::{self, Releases};
use crate::diff;
use crate::norm;

/// What the merge had to do to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Every release agrees about the code, so the newest release's text is the answer and there is
    /// no conditional in it at all.
    Same,
    /// Conditionals inside the file, around the parts that differ.
    Conditional,
    /// One branch holding the whole file per release, because every smaller region grew until it
    /// was the file: the releases disagree about where the file's own conditionals begin or end.
    PerRelease,
}

/// One merged header.
#[derive(Debug, Clone)]
pub struct Merged {
    /// The text to write.
    pub text: String,
    /// What had to be done to get it.
    pub kind: Kind,
    /// Whether at least one release does not have this header at all.
    pub guarded: bool,
    /// How many conditionals of ours are in it, not counting the guard.
    pub branches: usize,
    /// Whether glibc's own definition of `__GLIBC_MINOR__` was replaced in it.
    pub defined_the_macro: bool,
    /// What is wrong with it, which is empty for every file in a tree worth shipping.
    pub problems: Vec<String>,
}

/// What replaces glibc's own definition of the version macro.
///
/// A merged tree cannot define the minor version, because the whole point is that one tree serves
/// several, so the definition has to go. What goes in its place is the question this answers: an
/// empty space would mean a compiler that forgot to define it gets the oldest release's
/// declarations and no warning, and every `__GLIBC_PREREQ` in the program would quietly answer for
/// 2.28. So the definition is replaced by a check that there is one.
pub const GUARD: &str = "#ifndef __GLIBC_MINOR__\n# error \"this is rucc's merged glibc header \
                         tree, in which the compiler defines __GLIBC_MINOR__ from the target; see \
                         spec/cross-compile/08-sysroots.md section 8.3\"\n#endif\n";

/// Merges one header. `texts` has one entry per release, `None` where that release does not have it.
///
/// # Panics
///
/// When `texts` is not one entry per release, which is a mistake in the caller rather than
/// something a tree can be shaped like.
pub fn one(releases: &Releases, path: &str, texts: &[Option<&str>]) -> Result<Merged, String> {
    assert_eq!(texts.len(), releases.count(), "one text per release, present or not");
    for (n, text) in texts.iter().enumerate() {
        if text.is_some_and(cond::carries_mark) {
            return Err(format!(
                "{path}: the {} copy contains {}, so it has been through a merge already and \
                 merging it again would read its conditionals as ours",
                releases.spelled(n),
                cond::MARK
            ));
        }
    }

    let mut problems = Vec::new();
    let patched: Vec<Option<(String, bool)>> = texts.iter().map(|t| t.map(patch)).collect();
    let want: Vec<Option<&str>> =
        patched.iter().map(|p| p.as_ref().map(|(text, _)| text.as_str())).collect();
    let present: Vec<bool> = want.iter().map(Option::is_some).collect();
    let have: Vec<usize> = (0..releases.count()).filter(|&n| present[n]).collect();
    let (Some(&first), Some(&newest)) = (have.first(), have.last()) else {
        return Err(format!("{path}: no release has it"));
    };
    let cut: Vec<Option<norm::Pieces>> = want.iter().map(|t| t.map(norm::pieces)).collect();
    let pieces = |which: usize| cut[which].as_ref().expect("a release that has the file");

    // The lines every release has, and where each release has them. One release at a time against
    // what the ones before it agreed about, which is why what comes out is common to all of them.
    let mut spine: Vec<String> = pieces(first).keys().iter().map(|&k| k.to_owned()).collect();
    let mut at: Vec<Vec<usize>> = vec![(0..spine.len()).collect()];
    for &r in &have[1..] {
        let theirs = pieces(r).keys();
        let mine: Vec<&str> = spine.iter().map(String::as_str).collect();
        let pairs = diff::aligned(&mine, &theirs);
        let kept: Vec<String> = pairs.iter().map(|&(x, _)| spine[x].clone()).collect();
        for row in &mut at {
            *row = pairs.iter().map(|&(x, _)| row[x]).collect();
        }
        at.push(pairs.iter().map(|&(_, y)| y).collect());
        spine = kept;
    }

    // The file as slots. An even slot is the text between two spine lines, which is where a
    // difference lives, and an odd slot is a spine line, which every release has.
    let slots = 2 * spine.len() + 1;
    let by_slot: Vec<Vec<String>> = have
        .iter()
        .enumerate()
        .map(|(j, &r)| {
            let items = &pieces(r).items;
            (0..slots)
                .map(|slot| {
                    if slot % 2 == 1 {
                        return items[at[j][slot / 2]].text.clone();
                    }
                    let gap = slot / 2;
                    let from = if gap == 0 { 0 } else { at[j][gap - 1] + 1 };
                    let to = if gap == spine.len() { items.len() } else { at[j][gap] };
                    let mut text: String =
                        items[from..to].iter().map(|i| i.text.as_str()).collect();
                    if gap == spine.len() {
                        // The comments after the last line of code belong to the last gap.
                        text.push_str(&pieces(r).tail);
                    }
                    text
                })
                .collect()
        })
        .collect();
    let region = |lo: usize, hi: usize| grouped(&have, |j, _| by_slot[j][lo..=hi].concat());
    // Which slots have anything in them at all, so that a region holding all of them can be
    // reported as what it is, one copy of the file per release, however many empty slots the
    // alignment left around it.
    let content: Vec<bool> =
        (0..slots).map(|slot| by_slot.iter().any(|row| !row[slot].is_empty())).collect();
    let whole_file =
        |lo: usize, hi: usize| !content[..lo].contains(&true) && !content[hi + 1..].contains(&true);

    // What gets a conditional around it. A slot every release agrees about is written as it stands,
    // and a slot they do not agree about is wrapped together with as few of its neighbours as it
    // takes for every branch to stand on its own. Growing is not a search: a branch that reaches an
    // `#endif` whose `#if` is above it needs the text above it, and a branch that leaves an `#if`
    // open needs the text below it, so the branch says which way to grow and the region stops as
    // soon as nothing is reaching out of it.
    let mut regions: Vec<(usize, usize)> = Vec::new();
    let mut slot = 0;
    while slot < slots {
        let (mut lo, mut hi) = (slot, slot);
        loop {
            let groups = region(lo, hi);
            let reach =
                groups.iter().fold(Reach::default(), |all, (_, _, text)| all.with(&needs(text)));
            if groups.len() == 1 || !reach.out_of_it() {
                break;
            }
            // Growing below takes a slot this loop had not reached yet; growing above takes back a
            // region already decided, which is the one case where a decision is reconsidered.
            let below = reach.below && hi + 1 < slots;
            let above = reach.above && lo > 0;
            if below {
                hi += 1;
            } else if above {
                lo = regions.pop().expect("a region above to take back").0;
            } else if hi + 1 < slots {
                hi += 1;
            } else if lo > 0 {
                lo = regions.pop().expect("a region above to take back").0;
            } else {
                // The whole file, and its own conditionals do not balance. The branch is written
                // anyway and the check below is what says so.
                break;
            }
        }
        regions.push((lo, hi));
        slot = hi + 1;
    }

    let mut body = String::new();
    let mut branches = 0;
    let mut kind = Kind::Same;
    for &(lo, hi) in &regions {
        let groups = region(lo, hi);
        if let [(_, _, only)] = &groups[..] {
            body.push_str(only);
            continue;
        }
        // A group with nothing in it gets no branch. The releases in it are the ones that have
        // nothing here, and an empty `#if` would say that in three lines instead of none.
        let said: Vec<&(String, Vec<usize>, String)> =
            groups.iter().filter(|(_, _, text)| !text.is_empty()).collect();
        branches += 1;
        kind = if whole_file(lo, hi) { Kind::PerRelease } else { Kind::Conditional };
        for (n, (_, members, text)) in said.iter().enumerate() {
            line_end(&mut body);
            body.push_str(&cond::directive(
                if n == 0 { "if" } else { "elif" },
                Some(&condition(releases, members)),
            ));
            body.push_str(text);
            if !stands_alone(text) {
                problems.push(format!(
                    "{path}: the copies for {} do not have balanced conditionals, so no branch \
                     around them is right",
                    spelled(releases, members)
                ));
            }
        }
        line_end(&mut body);
        body.push_str(&cond::directive("endif", None));
    }

    // A header that arrives or goes away is still one file in the tree, and what it says for a
    // release that does not have it is what a missing header would have said.
    let guarded = have.len() != releases.count();
    let text = if have.len() == releases.count() {
        body
    } else {
        let mut text = cond::directive("if", Some(&condition(releases, &have)));
        text.push_str(&body);
        line_end(&mut text);
        text.push_str(&cond::directive("else", None));
        text.push_str(&format!(
            "#error \"rucc: {path} is not a header of this glibc release; it is in {}\"\n",
            spelled(releases, &have)
        ));
        text.push_str(&cond::directive("endif", None));
        text
    };

    // The check, which is the reason to believe any of the above.
    for (n, each) in want.iter().enumerate() {
        let Some(each) = each else { continue };
        match cond::evaluate(&text, releases.minors()[n]) {
            Ok(got) if norm::code(&got) == norm::code(each) => {}
            Ok(got) => problems.push(format!(
                "{path}: what this writes does not give the {} copy back, {}",
                releases.spelled(n),
                first_difference(&norm::code(&got), &norm::code(each))
            )),
            Err(why) => problems.push(format!(
                "{path}: reading back what this writes for {} failed: {why}",
                releases.spelled(n)
            )),
        }
    }
    if kind == Kind::Same && !guarded {
        // Nothing was written around anything, so this is a copy and the bytes say so.
        let same = want[newest].unwrap_or_default();
        if text.trim_end_matches('\n') != same.trim_end_matches('\n') {
            problems.push(format!(
                "{path}: no conditional was needed and the text still is not the {} copy",
                releases.spelled(newest)
            ));
        }
    }

    Ok(Merged {
        text,
        kind,
        guarded,
        branches,
        defined_the_macro: patched.iter().flatten().any(|(_, did)| *did),
        problems,
    })
}

/// The releases grouped by the code of what `text` gives for each of them, in the order their
/// oldest member comes in, with the newest member's real text kept for each group.
///
/// Grouping by the code rather than by the bytes is what keeps a copyright year from becoming a
/// conditional, and keeping the newest member's text is what keeps the tree reading like the newest
/// release rather than like a patchwork.
fn grouped(
    have: &[usize],
    mut text: impl FnMut(usize, usize) -> String,
) -> Vec<(String, Vec<usize>, String)> {
    let mut groups: Vec<(String, Vec<usize>, String)> = Vec::new();
    for (j, &r) in have.iter().enumerate() {
        let text = text(j, r);
        let code = norm::code(&text);
        match groups.iter_mut().find(|group| group.0 == code) {
            Some(group) => {
                group.1.push(r);
                group.2 = text;
            }
            None => groups.push((code, vec![r], text)),
        }
    }
    groups
}

/// The condition for a set of releases named by index.
fn condition(releases: &Releases, members: &[usize]) -> String {
    let mut flags = vec![false; releases.count()];
    for &m in members {
        flags[m] = true;
    }
    // Every release is a condition of its own only when something else in the file distinguishes
    // them, so this is never asked about the whole set.
    releases.condition(&flags).unwrap_or_else(|| "1".to_owned())
}

/// A set of releases, for a message.
fn spelled(releases: &Releases, members: &[usize]) -> String {
    members.iter().map(|&m| releases.spelled(m)).collect::<Vec<_>>().join(" ")
}

/// Whether this text can have a conditional wrapped around it.
///
/// It can when its own conditional directives balance and none of them continues one from outside
/// it. An `#endif` with nothing above it would close ours, and so would an `#else`, which is the
/// second of the hazards in the module documentation.
fn stands_alone(text: &str) -> bool {
    !needs(text).out_of_it()
}

/// Which way a text reaches out of itself, which is which way a region around it has to grow.
#[derive(Debug, Default, Clone, Copy)]
struct Reach {
    /// It closes or continues a conditional opened above it, so the region has to start higher up.
    above: bool,
    /// It leaves a conditional of its own open, so the region has to end further down.
    below: bool,
}

impl Reach {
    /// Both of them, because a region is as big as its neediest branch.
    fn with(self, other: &Reach) -> Self {
        Self { above: self.above || other.above, below: self.below || other.below }
    }

    /// Whether it reaches out at all, which is the same question `stands_alone` asks.
    fn out_of_it(self) -> bool {
        self.above || self.below
    }
}

/// What this text would need around it before a conditional of ours could wrap it.
///
/// The walk is over the code, so a directive inside a comment is not one. An `#endif` that takes the
/// depth below zero is closing somebody else's `#if` and so is an `#else` at depth zero, and both
/// are answered by starting the region higher up. Depth left above zero at the end is an `#if` of
/// the file's own that nothing here closes, and that is answered by ending the region further down.
fn needs(text: &str) -> Reach {
    let mut depth = 0i32;
    let mut reach = Reach::default();
    for line in norm::code(text).lines() {
        let Some(rest) = line.trim_start().strip_prefix('#') else { continue };
        let rest = rest.trim_start();
        if rest.starts_with("if") {
            depth += 1;
        } else if rest.starts_with("endif") {
            depth -= 1;
            if depth < 0 {
                reach.above = true;
                depth = 0;
            }
        } else if (rest.starts_with("else") || rest.starts_with("elif")) && depth == 0 {
            reach.above = true;
        }
    }
    if depth > 0 {
        reach.below = true;
    }
    reach
}

/// glibc's own definition of the version macro, replaced by the check that there is one.
fn patch(text: &str) -> (String, bool) {
    let cut = norm::pieces(text);
    if !cut.items.iter().any(|item| defines_the_macro(&item.key)) {
        return (text.to_owned(), false);
    }
    let mut out = String::with_capacity(text.len());
    for item in &cut.items {
        if defines_the_macro(&item.key) {
            // The comment above it stays, because it is glibc's comment about the version macros
            // and it is still true.
            out.push_str(&item.text[..item.code_at]);
            out.push_str(GUARD);
        } else {
            out.push_str(&item.text);
        }
    }
    out.push_str(&cut.tail);
    (out, true)
}

/// Whether this line is what defines the version macro.
fn defines_the_macro(key: &str) -> bool {
    let Some(rest) = key.strip_prefix('#') else { return false };
    let Some(rest) = rest.trim_start().strip_prefix("define") else { return false };
    let Some(rest) = rest.trim_start().strip_prefix(cond::MACRO) else { return false };
    let value = rest.trim();
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}

/// A newline, when the text does not already end in one, so a directive starts its own line.
fn line_end(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Where two texts first differ, for a message somebody has to act on.
fn first_difference(got: &str, want: &str) -> String {
    for (n, (left, right)) in got.lines().zip(want.lines()).enumerate() {
        if left != right {
            return format!("at line {} of the code: {} against {}", n + 1, cut(left), cut(right));
        }
    }
    format!("{} lines of code against {}", got.lines().count(), want.lines().count())
}

/// Enough of a line to recognize it, and not a screen of it.
fn cut(line: &str) -> String {
    let line = line.trim();
    if line.chars().count() <= 60 {
        return format!("`{line}`");
    }
    format!("`{}...`", line.chars().take(57).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn releases() -> Releases {
        Releases::new(vec![28, 31, 34]).expect("ascending")
    }

    /// The merge, and then the check on it that matters: what it wrote gives every input back.
    fn merged(texts: &[Option<&str>]) -> Merged {
        let all = releases();
        let out = one(&all, "sys/thing.h", texts).expect("a tree nobody merged before");
        assert_eq!(out.problems, Vec::<String>::new());
        for (n, want) in texts.iter().enumerate() {
            if let Some(want) = want {
                let got = cond::evaluate(&out.text, all.minors()[n]).expect("our own conditionals");
                assert_eq!(norm::code(&got), norm::code(want), "the 2.{} copy", all.minors()[n]);
            }
        }
        out
    }

    #[test]
    fn three_copies_of_one_file_are_that_file() {
        let text = "#ifndef _THING_H\n#define _THING_H 1\nint f (void);\n#endif\n";
        let out = merged(&[Some(text), Some(text), Some(text)]);
        assert_eq!(out.kind, Kind::Same);
        assert_eq!(out.text, text);
        assert_eq!(out.branches, 0);
        assert!(!out.guarded);
    }

    #[test]
    fn a_year_in_a_comment_is_not_worth_a_conditional() {
        let old = "/* Copyright (C) 2018 FSF. */\nint f (void);\n";
        let new = "/* Copyright (C) 2024 FSF. */\nint f (void);\n";
        let out = merged(&[Some(old), Some(old), Some(new)]);
        assert_eq!(out.kind, Kind::Same);
        // The newest release's text, which is how the tree ends up reading like one release.
        assert_eq!(out.text, new);
    }

    #[test]
    fn a_declaration_added_in_the_newest_release_is_behind_a_conditional() {
        let old = "int f (void);\n";
        let new = "int f (void);\nint g (void);\n";
        let out = merged(&[Some(old), Some(old), Some(new)]);
        assert_eq!(out.kind, Kind::Conditional);
        assert_eq!(out.branches, 1);
        // One branch and not two, because the releases without the line have nothing to put in one.
        assert_eq!(
            out.text,
            "int f (void);\n#if __GLIBC_MINOR__ >= 34 /* rucc */\nint g (void);\n#endif /* rucc */\n"
        );
    }

    #[test]
    fn a_declaration_removed_in_the_newest_release_is_behind_one_too() {
        let old = "int f (void);\nint gone (void);\n";
        let new = "int f (void);\n";
        let out = merged(&[Some(old), Some(old), Some(new)]);
        assert_eq!(out.kind, Kind::Conditional);
        assert!(out.text.contains("#if __GLIBC_MINOR__ < 34 /* rucc */"), "{}", out.text);
    }

    #[test]
    fn a_constant_that_changed_value_is_one_conditional_with_two_branches() {
        let out = merged(&[
            Some("#define _STAT_VER 1\n"),
            Some("#define _STAT_VER 1\n"),
            Some("#define _STAT_VER 3\n"),
        ]);
        assert_eq!(out.branches, 1);
        assert_eq!(out.text.matches("#elif").count(), 1);
    }

    #[test]
    fn a_header_that_arrives_later_says_so_for_the_releases_without_it() {
        let out = merged(&[None, None, Some("int f (void);\n")]);
        assert!(out.guarded);
        assert!(out.text.starts_with("#if __GLIBC_MINOR__ >= 34 /* rucc */"), "{}", out.text);
        assert!(
            out.text.contains(
                "#error \"rucc: sys/thing.h is not a header of this glibc \
                                   release; it is in 2.34\""
            ),
            "{}",
            out.text
        );
        // A release that does not have it gets the error and nothing else.
        let gone = cond::evaluate(&out.text, 28).expect("ours");
        assert!(gone.contains("#error"), "{gone}");
        assert!(!gone.contains("int f (void);"), "{gone}");
    }

    #[test]
    fn a_header_that_went_away_is_the_same_the_other_way_round() {
        let out = merged(&[Some("int f (void);\n"), Some("int f (void);\n"), None]);
        assert!(out.guarded);
        assert!(out.text.starts_with("#if __GLIBC_MINOR__ < 34 /* rucc */"), "{}", out.text);
    }

    /// A region that differs and sits inside one of the file's own conditionals is not the hazard:
    /// a branch there closes nothing it did not open.
    #[test]
    fn a_region_inside_the_files_own_conditional_is_left_where_it_is() {
        let old = "#ifdef __USE_GNU\nint f (void);\n#endif\n";
        let new = "#ifdef __USE_GNU\nint f (void);\nint g (void);\n#endif\nint h (void);\n";
        let out = merged(&[Some(old), Some(old), Some(new)]);
        assert_eq!(out.kind, Kind::Conditional);
        // One for the line added inside the file's own conditional and one for the line added
        // after it, which is two places rather than one thing in two places.
        assert_eq!(out.branches, 2);
        assert!(out.text.contains("#ifdef __USE_GNU\n"), "{}", out.text);
        // The file's own conditional is still one `#ifdef` and one `#endif` in both releases.
        for release in [28, 34] {
            let got = cond::evaluate(&out.text, release).expect("ours");
            assert_eq!(got.matches("#ifdef __USE_GNU").count(), 1, "{got}");
            assert_eq!(got.matches("#endif").count(), 1, "{got}");
        }
    }

    /// A condition the releases changed cannot be wrapped on its own, because the branch holding
    /// the old `#if` leaves it open. The region grows until it holds the block and stops there,
    /// which is the whole point of growing rather than escalating.
    #[test]
    fn a_changed_condition_takes_its_block_with_it_and_not_the_file() {
        let old = "int before (void);\n#ifdef A\nint f (void);\n#endif\nint after (void);\n";
        let new = "int before (void);\n#if defined A || defined B\nint f (void);\n#endif\n\
                   int after (void);\n";
        let out = merged(&[Some(old), Some(old), Some(new)]);
        assert_eq!(out.kind, Kind::Conditional);
        assert_eq!(out.branches, 1);
        // The lines either side of the block are written once, so the region was the block.
        assert_eq!(out.text.matches("int before (void);").count(), 1, "{}", out.text);
        assert_eq!(out.text.matches("int after (void);").count(), 1, "{}", out.text);
        assert_eq!(out.text.matches("int f (void);").count(), 2, "{}", out.text);
    }

    /// And when growing cannot stop short of the file, it does not: these two releases disagree
    /// about which of their own conditionals contains the other, so no region inside the file has
    /// branches that stand on their own.
    #[test]
    fn a_file_whose_conditionals_nest_differently_is_one_copy_per_release() {
        let old = "#if A\nint f (void);\n#endif\n#if B\nint g (void);\n#endif\n";
        let new = "#if A\nint f (void);\n#if B\nint g (void);\n#endif\n#endif\n";
        let out = merged(&[Some(old), Some(old), Some(new)]);
        assert_eq!(out.kind, Kind::PerRelease, "{}", out.text);
        assert_eq!(out.branches, 1);
        assert!(out.text.starts_with("#if __GLIBC_MINOR__ < 34 /* rucc */"), "{}", out.text);
        assert_eq!(out.text.matches("int f (void);").count(), 2, "{}", out.text);
        assert!(out.problems.is_empty(), "{:?}", out.problems);
    }

    #[test]
    fn the_definition_of_the_version_macro_is_replaced_by_the_check_for_one() {
        let all = releases();
        let texts: Vec<String> = all
            .minors()
            .iter()
            .map(|m| format!("#define __GLIBC__ 2\n#define\t__GLIBC_MINOR__\t{m}\nint f (void);\n"))
            .collect();
        let given: Vec<Option<&str>> = texts.iter().map(|t| Some(t.as_str())).collect();
        let out = one(&all, "features.h", &given).expect("not merged before");
        assert_eq!(out.problems, Vec::<String>::new());
        assert!(out.defined_the_macro);
        assert_eq!(out.kind, Kind::Same, "{}", out.text);
        assert!(!out.text.contains("#define\t__GLIBC_MINOR__"), "{}", out.text);
        assert!(out.text.contains("#define __GLIBC__ 2"), "{}", out.text);
        assert!(out.text.contains("#ifndef __GLIBC_MINOR__"), "{}", out.text);
        assert!(out.text.contains("# error"), "{}", out.text);
    }

    /// A mention of the macro in a comment is not a definition of it.
    #[test]
    fn a_comment_about_the_version_macro_is_left_alone() {
        let text = "/* #define __GLIBC_MINOR__ 44 is what glibc does. */\nint f (void);\n";
        let out = merged(&[Some(text), Some(text), Some(text)]);
        assert!(!out.defined_the_macro);
        assert_eq!(out.text, text);
    }

    #[test]
    fn a_tree_that_has_been_merged_once_is_refused() {
        let all = releases();
        let text = format!("int f (void);\n{}", cond::directive("endif", None));
        let why = one(&all, "sys/thing.h", &[Some(&text), Some(&text), Some(&text)])
            .expect_err("it carries the marker");
        assert!(why.contains("through a merge already"), "{why}");
    }

    #[test]
    fn a_file_no_release_has_is_an_error_rather_than_an_empty_file() {
        assert!(one(&releases(), "sys/thing.h", &[None, None, None]).is_err());
    }

    /// The merge can cut between logical lines only, so a macro whose body changed is one piece and
    /// the conditional lands around the whole definition.
    #[test]
    fn a_continued_macro_that_changed_is_replaced_whole() {
        let old = "#define F(a) \\\n  ((a) + 1)\nint f (void);\n";
        let new = "#define F(a) \\\n  ((a) + 2)\nint f (void);\n";
        let out = merged(&[Some(old), Some(old), Some(new)]);
        assert_eq!(out.kind, Kind::Conditional);
        // Neither branch is half a definition.
        for release in [28, 34] {
            let got = cond::evaluate(&out.text, release).expect("ours");
            assert_eq!(got.matches("#define F(a)").count(), 1, "{got}");
        }
    }

    #[test]
    fn a_file_with_no_trailing_newline_still_gets_whole_directives() {
        let out = merged(&[Some("int f (void);"), Some("int f (void);"), Some("int g (void);")]);
        for line in out.text.lines() {
            assert!(!line.contains("#endif") || line.trim_start().starts_with('#'), "{line}");
        }
    }

    #[test]
    fn which_way_a_branch_reaches_out_of_itself() {
        assert!(needs("#endif\n").above);
        assert!(needs("#else\nint f (void);\n").above);
        assert!(needs("#ifdef A\nint f (void);\n").below);
        assert!(!needs("#ifdef A\nint f (void);\n#endif\n").out_of_it());
        // An `#endif` that closes somebody else's and then an `#if` of its own reaches both ways.
        let both = needs("#endif\n#ifdef A\nint f (void);\n");
        assert!(both.above && both.below);
    }

    #[test]
    fn what_stands_alone_and_what_does_not() {
        assert!(stands_alone("int f (void);\n"));
        assert!(stands_alone("#ifdef A\nint f (void);\n#endif\n"));
        assert!(!stands_alone("#endif\n"));
        assert!(!stands_alone("#else\nint f (void);\n"));
        assert!(!stands_alone("#ifdef A\nint f (void);\n"));
        // A directive inside a comment is not a directive.
        assert!(stands_alone("/* #endif */\nint f (void);\n"));
    }
}
