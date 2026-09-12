//! Lining two releases of the same header up, so the merge can see what moved and what did not.
//!
//! The merge needs, for two sequences of code lines, the pairs that are the same line. What it does
//! with them is in `merge.rs`; this is only the lining up, and it is the part where an easy
//! implementation is too slow on the files that matter. `elf.h` is four thousand lines and changes
//! at every release, and the textbook table of one cell per pair of lines is sixteen million cells
//! for one file and one release, eight times over.
//!
//! So three steps, cheapest first, which is what every diff worth using does:
//!
//! 1. The common prefix and the common suffix. Two releases of a header agree about almost all of
//!    it, and agreeing at the ends is free to notice.
//! 2. The lines that appear exactly once in each of what is left. Those are anchors: a line that is
//!    unique on both sides and in increasing order on both sides cannot be anything but itself.
//!    This is Bram Cohen's patience diff, and the reason it is right for header text rather than
//!    merely fast is that it refuses to match the eighth `#endif` with the third one.
//! 3. The table, for what is left between two anchors, which after the first two steps is small.
//!    A region with no unique line at all and more cells than the cap is left unmatched, which
//!    makes the merge write that region out per release. That is coarser and never wrong.
//!
//! Every step narrows the problem and no step can match two lines that are not equal, so the worst
//! this can do is a bigger tree than necessary.

/// The most cells the table is allowed, which is four megabytes of `u32`.
///
/// A region this big with no line unique on both sides is not a header that moved, it is two
/// different files, and merging those line by line produces something nobody can review.
const CELLS: usize = 1 << 20;

/// The pairs of positions that hold the same line, in increasing order on both sides.
pub fn aligned(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut work = vec![(0, a.len(), 0, b.len())];
    while let Some((mut a0, mut a1, mut b0, mut b1)) = work.pop() {
        while a0 < a1 && b0 < b1 && a[a0] == b[b0] {
            out.push((a0, b0));
            a0 += 1;
            b0 += 1;
        }
        while a1 > a0 && b1 > b0 && a[a1 - 1] == b[b1 - 1] {
            a1 -= 1;
            b1 -= 1;
            out.push((a1, b1));
        }
        if a0 >= a1 || b0 >= b1 {
            continue;
        }
        let anchors = anchors(&a[a0..a1], &b[b0..b1]);
        if anchors.is_empty() {
            if (a1 - a0).saturating_mul(b1 - b0) <= CELLS {
                out.extend(
                    table(&a[a0..a1], &b[b0..b1]).into_iter().map(|(x, y)| (a0 + x, b0 + y)),
                );
            }
            continue;
        }
        let (mut at, mut bt) = (a0, b0);
        for (x, y) in anchors {
            let (ax, by) = (a0 + x, b0 + y);
            work.push((at, ax, bt, by));
            out.push((ax, by));
            at = ax + 1;
            bt = by + 1;
        }
        work.push((at, a1, bt, b1));
    }
    out.sort_unstable();
    out
}

/// The lines that appear exactly once on each side, in an order both sides agree about.
fn anchors(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let once_in_a = once(a);
    let once_in_b = once(b);
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for (x, line) in a.iter().enumerate() {
        if once_in_a.get(line) == Some(&Some(x)) {
            if let Some(&Some(y)) = once_in_b.get(line) {
                pairs.push((x, y));
            }
        }
    }
    increasing(&pairs)
}

/// Where each line is, for the lines that are there once, and `None` for the rest.
fn once<'a>(lines: &[&'a str]) -> std::collections::HashMap<&'a str, Option<usize>> {
    let mut seen: std::collections::HashMap<&str, Option<usize>> = std::collections::HashMap::new();
    for (at, line) in lines.iter().enumerate() {
        seen.entry(line).and_modify(|e| *e = None).or_insert(Some(at));
    }
    seen
}

/// The longest run of pairs that increases on the second side as well as the first.
///
/// Patience sorting, with a back pointer per pair, which is the standard way and is here because
/// two lines unique on both sides can still have swapped places between releases, and taking both
/// of them would claim an order the file does not have.
fn increasing(pairs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    if pairs.is_empty() {
        return Vec::new();
    }
    // `piles[k]` is the index into `pairs` of the smallest second element ending a run of k + 1.
    let mut piles: Vec<usize> = Vec::new();
    let mut came_from: Vec<Option<usize>> = vec![None; pairs.len()];
    for (at, &(_, y)) in pairs.iter().enumerate() {
        let pile = piles.partition_point(|&p| pairs[p].1 < y);
        came_from[at] = if pile == 0 { None } else { Some(piles[pile - 1]) };
        if pile == piles.len() {
            piles.push(at);
        } else {
            piles[pile] = at;
        }
    }
    let mut run = Vec::with_capacity(piles.len());
    let mut at = piles.last().copied();
    while let Some(i) = at {
        run.push(pairs[i]);
        at = came_from[i];
    }
    run.reverse();
    run
}

/// The longest common subsequence of two short sequences, by the table.
fn table(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let (rows, cols) = (a.len() + 1, b.len() + 1);
    let mut best = vec![0u32; rows * cols];
    for x in (0..a.len()).rev() {
        for y in (0..b.len()).rev() {
            best[x * cols + y] = if a[x] == b[y] {
                best[(x + 1) * cols + y + 1] + 1
            } else {
                best[(x + 1) * cols + y].max(best[x * cols + y + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut x, mut y) = (0, 0);
    while x < a.len() && y < b.len() {
        if a[x] == b[y] {
            out.push((x, y));
            x += 1;
            y += 1;
        } else if best[(x + 1) * cols + y] >= best[x * cols + y + 1] {
            x += 1;
        } else {
            y += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<&str> {
        text.split_whitespace().collect()
    }

    /// The pairs are in increasing order on both sides and every pair is two equal lines, which is
    /// what the merge assumes and is the only thing that makes the output legal rather than small.
    fn sound(a: &[&str], b: &[&str], pairs: &[(usize, usize)]) {
        for (n, &(x, y)) in pairs.iter().enumerate() {
            assert_eq!(a[x], b[y], "pair {n} is not two equal lines");
            if n > 0 {
                assert!(pairs[n - 1].0 < x && pairs[n - 1].1 < y, "pair {n} goes backwards");
            }
        }
    }

    #[test]
    fn two_equal_files_line_up_entirely() {
        let (a, b) = (lines("one two three"), lines("one two three"));
        let pairs = aligned(&a, &b);
        sound(&a, &b, &pairs);
        assert_eq!(pairs, vec![(0, 0), (1, 1), (2, 2)]);
    }

    #[test]
    fn a_line_added_in_the_middle_is_the_only_thing_unmatched() {
        let (a, b) = (lines("one two five"), lines("one two three five"));
        let pairs = aligned(&a, &b);
        sound(&a, &b, &pairs);
        assert_eq!(pairs, vec![(0, 0), (1, 1), (2, 3)]);
    }

    #[test]
    fn nothing_in_common_matches_nothing() {
        let (a, b) = (lines("one two"), lines("three four"));
        assert_eq!(aligned(&a, &b), Vec::new());
    }

    #[test]
    fn an_empty_side_matches_nothing() {
        assert_eq!(aligned(&lines("one"), &[]), Vec::new());
        assert_eq!(aligned(&[], &lines("one")), Vec::new());
    }

    /// The case patience diff is for: the repeated line is not an anchor, so the unique ones
    /// decide, and the `#endif` that matches is the one in the same place rather than the first.
    #[test]
    fn a_repeated_line_does_not_drag_the_alignment_out_of_order() {
        let a = lines("#if A x #endif #if B y #endif");
        let b = lines("#if B y #endif");
        let pairs = aligned(&a, &b);
        sound(&a, &b, &pairs);
        let matched: Vec<&str> = pairs.iter().map(|&(x, _)| a[x]).collect();
        assert_eq!(matched, vec!["#if", "B", "y", "#endif"]);
    }

    /// Two lines that swapped places cannot both be matched, or the pairs would not increase.
    #[test]
    fn a_swap_keeps_one_of_the_two() {
        let (a, b) = (lines("head one two tail"), lines("head two one tail"));
        let pairs = aligned(&a, &b);
        sound(&a, &b, &pairs);
        assert_eq!(pairs.len(), 3);
    }

    #[test]
    fn a_file_that_grew_at_both_ends_keeps_its_middle() {
        let (a, b) = (lines("middle"), lines("before middle after"));
        let pairs = aligned(&a, &b);
        sound(&a, &b, &pairs);
        assert_eq!(pairs, vec![(0, 1)]);
    }

    /// Long enough that the table is not what did the work, with a change in the middle.
    #[test]
    fn a_long_file_with_one_change_in_the_middle() {
        let left: Vec<String> = (0..5000).map(|n| format!("line {n}")).collect();
        let mut right = left.clone();
        right[2500] = "line changed".to_owned();
        let a: Vec<&str> = left.iter().map(String::as_str).collect();
        let b: Vec<&str> = right.iter().map(String::as_str).collect();
        let pairs = aligned(&a, &b);
        sound(&a, &b, &pairs);
        assert_eq!(pairs.len(), 4999);
    }

    /// A region with no line unique on both sides and more cells than the cap is left alone, which
    /// the merge turns into one branch per release rather than into a guess.
    #[test]
    fn a_region_past_the_cap_with_no_anchor_is_left_unmatched() {
        let side: Vec<String> = (0..2000).map(|n| format!("{}", n % 2)).collect();
        let other: Vec<String> = (0..2000).map(|n| format!("{}", (n + 1) % 2)).collect();
        let a: Vec<&str> = side.iter().map(String::as_str).collect();
        let b: Vec<&str> = other.iter().map(String::as_str).collect();
        assert!(a.len() * b.len() > CELLS);
        assert_eq!(aligned(&a, &b), Vec::new());
    }
}
