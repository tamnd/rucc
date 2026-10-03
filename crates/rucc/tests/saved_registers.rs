//! A value that crosses no call is not kept in a register the callee has to save.
//!
//! gcc puts a value whose life holds no call in a register a call destroys, and only pushes what
//! has to survive one. A loop that leaves through a call is the common case: the loop's values are
//! dead by the time the call is made, so the call destroying their registers costs nothing. rucc
//! used to count that call against every register it destroys over the whole of the loop and so
//! kept the loop counter in `rbx`, which is a push and a pop in a function that needs neither. See
//! tamnd/rucc#2202.

use std::path::PathBuf;
use std::process::Command;

/// The assembly for that source at `-O2` on x86-64, under a directory of its own.
fn assembly(what: &str, source: &str) -> String {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("rucc-saved-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let done = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    String::from_utf8(done.stdout).expect("the listing is text")
}

/// The instructions of one function, without directives or labels.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.contains(".cfi_endproc") && !line.starts_with(".Lfunc_end"))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('.') && !line.ends_with(':'))
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

const LOOPS: &str = "typedef struct List { int length; int *elems; } List;
extern List *list_delete_nth(List *list, int n);
extern const char *lookup_name(int id);

List *list_delete_value(List *list, int value)
{
	for (int i = 0; i < list->length; i++)
		if (list->elems[i] == value)
			return list_delete_nth(list, i);
	return list;
}

const char *first_match(const int *ids, int n, int lo, int hi)
{
	for (int i = 0; i < n; i++)
		if (ids[i] >= lo && ids[i] <= hi)
			return lookup_name(ids[i]);
	return 0;
}
";

#[test]
fn a_loop_that_leaves_through_a_call_pushes_nothing() {
    let text = assembly("loops", LOOPS);
    for name in ["list_delete_value", "first_match"] {
        let body = body(&text, name);
        assert!(!body.is_empty(), "{name} is in the listing:\n{text}");
        assert!(!body.iter().any(|line| line.starts_with("push")), "{name}: {body:#?}");
    }
}
