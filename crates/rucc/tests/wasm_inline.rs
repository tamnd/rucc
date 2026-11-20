//! The second inliner on wasm32 at `-O2`. A call in a loop is held to the hinted limit when its
//! callee is `static`, every call to the callee is in that caller, and the caller is not large,
//! because then the callee's own copy goes away. The shape is the merge of SQLite's sorter, which
//! clang inlines and gcc's limits leave as a call. A native target keeps gcc's decision.
//!
//! Design: #2866.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// A merge of two sorted lists, over the limit for a call that has no hint, with both of its
/// calls in loops of `sort`.
const SORT: &str = "struct rec { struct rec *next; int key; };\n\
    struct task { int (*cmp)(const struct rec *, const struct rec *); int compares; int merges; };\n\
    static struct rec *merge(struct task *t, struct rec *a, struct rec *b)\n\
    {\n\
      struct rec *head = 0, **pp = &head;\n\
      while (a && b) {\n\
        int c = t->cmp(a, b);\n\
        t->compares++;\n\
        if (c == 0) c = a->key - b->key;\n\
        if (c <= 0) { *pp = a; pp = &a->next; a = a->next; }\n\
        else { *pp = b; pp = &b->next; b = b->next; }\n\
      }\n\
      *pp = a ? a : b;\n\
      t->merges++;\n\
      return head;\n\
    }\n\
    struct rec *sort(struct task *t, struct rec *list)\n\
    {\n\
      struct rec *slot[64] = {0};\n\
      while (list) {\n\
        struct rec *next = list->next;\n\
        int i = 0;\n\
        list->next = 0;\n\
        for (; slot[i]; i++) { list = merge(t, slot[i], list); slot[i] = 0; }\n\
        slot[i] = list;\n\
        list = next;\n\
      }\n\
      for (int i = 0; i < 64; i++) list = merge(t, slot[i], list);\n\
      return list;\n\
    }\n";

/// A third call to `merge`, from another function, which keeps the callee's own copy.
const ONE: &str =
    "struct rec *one(struct task *t, struct rec *a) { return merge(t, a, a->next); }\n";

/// The assembly of rucc for `source` on `target` at `-O2`.
fn assembly(source: &str, target: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .arg(format!("--target={target}"))
        .args(["-O2", "-x", "c", "-", "-S", "-o", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    child.stdin.take().expect("a pipe for the input").write_all(source.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("the compiler finished");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

/// How many calls and tail jumps to `merge` the listing has.
fn merges(listing: &str) -> usize {
    listing
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .filter(|words| matches!(words.as_slice(), ["call" | "jmp", "merge"]))
        .count()
}

#[test]
fn a_callee_called_only_in_loops_of_one_caller_is_copied_on_wasm() {
    let listing = assembly(SORT, "wasm32-wasip1");
    assert_eq!(merges(&listing), 0, "{listing}");
    assert!(!listing.contains("merge:"), "{listing}");
}

#[test]
fn the_same_callee_stays_a_call_on_x86_64() {
    let listing = assembly(SORT, "x86_64-unknown-linux-gnu");
    assert_eq!(merges(&listing), 2, "{listing}");
}

#[test]
fn a_callee_called_from_another_function_too_stays_a_call_on_wasm() {
    let listing = assembly(&format!("{SORT}{ONE}"), "wasm32-wasip1");
    assert_eq!(merges(&listing), 3, "{listing}");
}
