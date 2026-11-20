//! The client against the server of this crate, over TCP on the loopback address.

use std::io::ErrorKind;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

use rucc_9p::serve::{Tree, serve};
use rucc_9p::{Client, dial};

/// A server of `tree` on a free port, for one connection, and its address as a dial string.
fn start(tree: &Arc<Mutex<Tree>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    let port = listener.local_addr().expect("the port is known").port();
    let tree = Arc::clone(tree);
    thread::spawn(move || {
        let (stream, _) = listener.accept().expect("the client connects");
        serve(stream, &tree).expect("the session ends cleanly");
    });
    format!("tcp!127.0.0.1!{port}")
}

fn sample() -> Arc<Mutex<Tree>> {
    let mut tree = Tree::new();
    tree.insert("sys/include/libc.h", "#pragma lib \"libc.a\"\n");
    tree.insert("sys/include/u.h", "typedef long vlong;\n");
    tree.insert("amd64/include/u.h", "typedef long long vlong;\n");
    Arc::new(Mutex::new(tree))
}

#[test]
fn a_file_is_read_and_then_comes_from_the_cache() {
    let tree = sample();
    let mut c = Client::connect(dial(&start(&tree)).unwrap(), "glenda", "").unwrap();
    assert_eq!(&*c.read_file("/sys/include/libc.h").unwrap(), b"#pragma lib \"libc.a\"\n");
    assert_eq!(&*c.read_file("amd64/include/u.h").unwrap(), b"typedef long long vlong;\n");
    let before = c.stats();
    assert_eq!(before.cache_hits, 0);
    // A second read is a walk and a clunk, and no open or read.
    assert_eq!(&*c.read_file("sys/include/libc.h").unwrap(), b"#pragma lib \"libc.a\"\n");
    let after = c.stats();
    assert_eq!(after.cache_hits, 1);
    assert_eq!(after.round_trips - before.round_trips, 2);
    assert_eq!(after.bytes, before.bytes);
    // A change on the server gives a new version, and the client reads the new bytes.
    tree.lock().unwrap().insert("sys/include/libc.h", "#pragma lib \"/$M/lib/libc.a\"\n");
    assert_eq!(&*c.read_file("sys/include/libc.h").unwrap(), b"#pragma lib \"/$M/lib/libc.a\"\n");
    assert_eq!(c.stats().cache_hits, 1);
    assert!(c.stats().wait > std::time::Duration::ZERO);
}

#[test]
fn the_errors_have_the_kinds_of_the_host() {
    let tree = sample();
    let mut c = Client::connect(dial(&start(&tree)).unwrap(), "glenda", "").unwrap();
    let kind = |r: std::io::Result<_>| r.map(|_: Arc<[u8]>| ()).unwrap_err().kind();
    assert_eq!(kind(c.read_file("sys/include/stdio.h")), ErrorKind::NotFound);
    assert_eq!(kind(c.read_file("nosuch/include/u.h")), ErrorKind::NotFound);
    assert_eq!(kind(c.read_file("sys/include/u.h/x")), ErrorKind::NotFound);
    assert_eq!(kind(c.read_file("sys/include")), ErrorKind::IsADirectory);
    // The fids of the failed walks are given back, and the session goes on.
    assert_eq!(&*c.read_file("sys/include/u.h").unwrap(), b"typedef long vlong;\n");
}

#[test]
fn a_deep_path_and_a_large_file_take_more_than_one_request() {
    let tree = sample();
    let deep = (0..40).map(|i| format!("d{i}")).collect::<Vec<_>>().join("/") + "/f.h";
    let large: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    tree.lock().unwrap().insert(&deep, "deep\n");
    tree.lock().unwrap().insert("large.h", large.clone());
    let mut c = Client::connect(dial(&start(&tree)).unwrap(), "glenda", "").unwrap();
    assert_eq!(&*c.read_file(&deep).unwrap(), b"deep\n");
    assert_eq!(&*c.read_file(&format!("d0/d1/../d1/{}", &deep[6..])).unwrap(), b"deep\n");
    assert_eq!(&*c.read_file("large.h").unwrap(), &large[..]);
    assert_eq!(c.stat("large.h").unwrap().length, 300_000);
    let root = c.stat("/").unwrap();
    assert!(root.qid.is_dir());
}

#[test]
fn a_directory_gives_its_files() {
    let tree = sample();
    for i in 0..1000 {
        tree.lock().unwrap().insert(&format!("many/file-with-a-long-name-{i:04}.h"), "");
    }
    let mut c = Client::connect(dial(&start(&tree)).unwrap(), "glenda", "").unwrap();
    let names: Vec<String> =
        c.read_dir("sys/include").unwrap().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["libc.h", "u.h"]);
    let many = c.read_dir("many").unwrap();
    assert_eq!(many.len(), 1000);
    assert_eq!(many[999].name, "file-with-a-long-name-0999.h");
    assert_eq!(c.read_dir("sys/include/u.h").unwrap_err().kind(), ErrorKind::NotADirectory);
}
