//! Checks that no Rust in the tree uses a let chain.
//!
//! A let chain is `if let` or `while let` joined to another condition with `&&`, and it is stable
//! from Rust 1.88. The workspace says `rust-version = "1.85.0"`, which the `msrv` job in CI holds
//! it to, but that job needs a second toolchain and most merges go in on a local run that does not
//! have one. The newest compiler takes a let chain without a word, so one has reached main four
//! times, each time from a different change. See tamnd/rucc#1782.
//!
//! This is the same question asked without the old toolchain, so it runs anywhere `cargo test`
//! does. It reads the source rather than parsing it, which is enough because rustfmt leaves a
//! chain in one of two shapes: `&& let` somewhere in the condition, or a `&&` outside every
//! bracket after the `=` of an `if let` or a `while let`. The second is only a chain, since the
//! same `&&` without the `let` in front is not something 1.85 accepts either.

use std::fs;

use crate::{Error, Result, collect_rust, root};

/// The directories with Rust in them that the workspace builds.
const DIRS: &[&str] = &["crates", "runtime", "build-tools", "xtask"];

/// What a chain that starts with an ordinary condition looks like, spelled with an escape so that
/// this file does not trip its own check.
const AND_LET: &str = "\x26\x26 let ";

/// Checks every Rust file under [`DIRS`] for a let chain.
pub(crate) fn chains() -> Result<()> {
    let root = root();
    let mut files = Vec::new();
    for dir in DIRS {
        let dir = root.join(dir);
        if dir.is_dir() {
            collect_rust(&dir, &mut files)?;
        }
    }
    files.sort();
    let mut problems = Vec::new();
    for path in &files {
        let text = fs::read_to_string(path)?;
        let shown = path.strip_prefix(&root).unwrap_or(path).display();
        for line in chained(&text) {
            problems.push(format!(
                "{shown}:{line}: a let chain, which needs Rust 1.88 where the workspace promises \
                 1.85. Use `is_some_and`, `matches!` with a guard, or a nested `if`"
            ));
        }
    }
    if problems.is_empty() {
        println!("chains: {} Rust files, no let chains", files.len());
        Ok(())
    } else {
        Err(Error::Failed { task: "chains", problems })
    }
}

/// The line numbers, counting from one, where a let chain starts in this source.
fn chained(text: &str) -> Vec<usize> {
    let lines: Vec<&str> = text.lines().map(code).collect();
    let mut found = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        if line.contains(AND_LET) {
            found.push(at + 1);
            continue;
        }
        let trimmed = line.trim_start().trim_start_matches("} else ");
        if !(trimmed.starts_with("if let ") || trimmed.starts_with("while let ")) {
            continue;
        }
        // The condition, from here up to the brace that opens the body, which rustfmt may have
        // spread over several lines.
        let mut condition = String::new();
        for more in &lines[at..] {
            condition.push_str(more);
            condition.push('\n');
            if more.trim_end().ends_with('{') {
                break;
            }
        }
        if joined(&condition) {
            found.push(at + 1);
        }
    }
    found
}

/// Whether a condition that starts with `if let` or `while let` has a `&&` outside every bracket
/// after its `=`.
fn joined(condition: &str) -> bool {
    let bytes = condition.as_bytes();
    let mut depth = 0i32;
    let mut after = false;
    for (i, &b) in bytes.iter().enumerate() {
        let next = bytes.get(i + 1).copied();
        let before = i.checked_sub(1).map(|j| bytes[j]);
        match b {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            // The brace that opens the body is where the condition ends. One inside a bracket is a
            // struct literal or a block in an argument.
            b'{' if depth == 0 => return false,
            b'{' => depth += 1,
            b'}' => depth -= 1,
            b'=' if depth == 0
                && !matches!(next, Some(b'=' | b'>'))
                && !matches!(before, Some(b'=' | b'!' | b'<' | b'>')) =>
            {
                after = true;
            }
            b'&' if depth == 0 && after && next == Some(b'&') && before == Some(b' ') => {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// A line without its comment, so that prose about a chain is not taken for one.
fn code(line: &str) -> &str {
    if line.trim_start().starts_with("//") { "" } else { line }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The samples spell `&&` as `\x26\x26` so that this file does not trip its own check.

    #[test]
    fn a_let_after_a_condition_is_a_chain() {
        assert_eq!(chained("if ready \x26\x26 let Some(x) = next {\n}\n"), [1]);
    }

    #[test]
    fn a_condition_after_a_let_on_its_own_lines_is_a_chain() {
        let text = "fn f() {\n    if let Some(x) = next\n        \x26\x26 x > 3\n    {\n    }\n}\n";
        assert_eq!(chained(text), [2]);
    }

    #[test]
    fn a_condition_after_a_let_on_one_line_is_a_chain() {
        assert_eq!(chained("while let Some(x) = it.next() \x26\x26 x > 3 {\n}\n"), [1]);
    }

    #[test]
    fn a_double_reference_in_a_closure_is_not_a_chain() {
        let text = "if let Some(x) = list.iter().find(|\x26\x26(name, _)| name == want) {\n}\n";
        assert!(chained(text).is_empty());
    }

    #[test]
    fn a_conjunction_inside_an_argument_is_not_a_chain() {
        let text = "if let Some(x) = pick(\n    a\n        \x26\x26 b,\n) {\n}\n";
        assert!(chained(text).is_empty());
    }

    #[test]
    fn a_plain_if_is_not_a_chain() {
        assert!(chained("if a \x26\x26 b {\n}\nif let Some(x) = y {\n}\n").is_empty());
    }

    #[test]
    fn a_comment_about_a_chain_is_not_one() {
        assert!(chained("// if ready \x26\x26 let Some(x) = next\n").is_empty());
    }

    /// The tree itself, so that `cargo test` fails on a chain without anybody running the task.
    #[test]
    fn the_tree_has_no_let_chains() {
        chains().expect("a let chain in the tree");
    }
}
