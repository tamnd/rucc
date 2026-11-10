//! A sysroot on a 9P server, end to end.
//!
//! Design: tamnd/rucc#4153, and 07 of the rucc-plan9 plan.
//!
//! Each test starts the small server of `rucc-9p` on a free port with a tree in memory, and
//! runs the compiler with `--sysroot=9p:tcp!127.0.0.1!<port>`. The headers of the target and
//! the source file come from the server, and nothing of the tree is on the disk.

use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;

use rucc_9p::serve::{Tree, serve};

/// A server of `tree` on a free port for one connection, and the sysroot that names it.
fn start(tree: Tree) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().expect("the port is known").port();
    let tree = Arc::new(Mutex::new(tree));
    thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let _ = serve(stream, &tree);
        }
    });
    format!("9p:tcp!127.0.0.1!{port}")
}

/// A small Plan 9 tree: the two include directories of amd64 and one program.
fn plan9_tree() -> Tree {
    let mut tree = Tree::new();
    tree.insert("amd64/include/u.h", "typedef long long vlong;\ntypedef unsigned long ulong;\n");
    tree.insert(
        "sys/include/libc.h",
        "#pragma lib \"libc.a\"\nextern int print(char*, ...);\nextern ulong strlen(char*);\n",
    );
    tree.insert(
        "sys/src/cmd/hello/dat.h",
        "typedef struct Lock Lock;\nstruct Lock { int key; };\n",
    );
    tree.insert(
        "sys/src/cmd/hello/hello.c",
        "#include <u.h>\n#include <libc.h>\n#include \"dat.h\"\n#include <u.h>\n\
         struct Ref { Lock; long ref; };\n\
         vlong n(struct Ref *r) { return r->key + strlen(\"hi\"); }\n",
    );
    tree
}

fn rucc(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .output()
        .expect("the compiler is built before its own tests run")
}

#[test]
fn the_headers_and_the_source_come_from_the_server() {
    let root = start(plan9_tree());
    let sysroot = format!("--sysroot={root}");
    let source = format!("{root}/sys/src/cmd/hello/hello.c");
    let out =
        rucc(&["--target=x86_64-plan9", "-fkencc", "-fsyntax-only", "-time", &sysroot, &source]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    // The line of `-time` for the server: the reads, and `u.h` the second time from the cache.
    let line = err.lines().find(|l| l.starts_with("# 9p tcp!127.0.0.1!")).expect("a 9p line");
    assert!(line.ends_with(", 1 from the cache"), "{line}");
}

#[test]
fn a_missing_header_is_reported_with_the_server_in_its_path() {
    let mut tree = plan9_tree();
    tree.insert("sys/src/cmd/bad.c", "#include <draw.h>\n");
    let root = start(tree);
    let sysroot = format!("--sysroot={root}");
    let source = format!("{root}/sys/src/cmd/bad.c");
    let out = rucc(&["--target=x86_64-plan9", "-fkencc", "-fsyntax-only", &sysroot, &source]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(err.contains("bad.c:1:"), "{err}");
    assert!(err.contains("draw.h"), "{err}");
}

#[test]
fn a_server_that_is_not_there_is_one_error() {
    // A port that was free a moment ago, and now has no listener.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let sysroot = format!("--sysroot=9p:tcp!127.0.0.1!{port}");
    let out = rucc(&["--target=x86_64-plan9", "-fsyntax-only", &sysroot, "-x", "c", "/dev/null"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        err.starts_with("rucc: error: cannot read the sysroot from the 9P server tcp!127.0.0.1!"),
        "{err}"
    );
}
