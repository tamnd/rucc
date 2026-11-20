//! The acid text of `-a`, `-aa` and `-n`, which kencc writes in place of an object.
//!
//! The text that is expected here is what the `6c` of 9front writes for the same files. A record
//! writes its `aggr` and its `defn` when it closes, and an object of a record type, or of a pointer
//! to one, writes a `complex` line. With `-aa`, only what the named file declares is written, and
//! with `-n` the text goes to a file whose name ends in `.acid`. Links are a Unix thing, so this is
//! compiled on Unix only.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ONE_H: &str = r#"typedef unsigned char uchar;
typedef unsigned short ushort;
typedef unsigned int uint;
typedef unsigned long ulong;
typedef long long vlong;
typedef unsigned long long uvlong;
typedef struct Lock Lock;
struct Lock
{
	long	key;
	ulong	pc;
};
"#;

const ONE_C: &str = r#"#include "one.h"

typedef struct Node Node;
typedef struct Pair Pair;
typedef struct Fwd Fwd;

enum
{
	Red,
	Green = 5,
	Blue,
	Neg = -3,
	head = 7,
};

struct Pair
{
	int	a;
	uchar	b;
	short	c;
	ushort	d;
	uint	e;
	long	f;
	ulong	g;
	vlong	h;
	uvlong	i;
	float	j;
	double	k;
	char	l[10];
	Fwd	*fwd;
	int	(*fn)(int);
	enum { X1, X2 } en;
	uint	bits1:3;
	uint	bits2:30;
	int	tail;
};

struct Node
{
	Lock;
	Node	*next;
	Node	**pp;
	Pair	pair;
	struct {
		int	x;
		int	y;
	};
	struct {
		int	u;
	} named;
	union {
		long	l;
		double	d;
	} un;
	int	Blue;
	int	gp;
};

struct aggr
{
	int	loop;
};

struct Empty;

extern register Node *m;
Pair gp;
Node *list;
static Pair spair;
extern Node enode;
Pair arr[3];
struct aggr agg;

void
f(Node *n, Pair p, int x)
{
	Pair lp;
	static Node sn;
	Node *q;

	{
		static Pair inner;
		Pair blk;
	}
	for(x = 0; x < 2; x++){
		static Node ins;
	}
	{
		struct Local { int z; } loc;
	}
}

struct Fwd
{
	char	*name;
	Pair	*sn;
};

Fwd fwdvar;

int
g(Fwd *fw)
{
	extern Pair gp;
	static Fwd sfw;
	return fw->name != 0 && sfw.name == 0;
}
"#;

const TWO_C: &str = r#"#include "one.h"

struct Two
{
	Lock	l;
	int	n;
};

struct Two two;
"#;

/// What `one.h` declares, which `-aa` does not write.
const LOCK: &str = r#"sizeofLock = 8;
aggr Lock
{
	'D' 0 key;
	'U' 4 pc;
};

defn
Lock(addr) {
	complex Lock addr;
	print("	key	", addr.key, "\n");
	print("	pc	", addr.pc, "\n");
};

"#;

/// What `one.c` declares.
const ONE: &str = r#"Red = 0;
Green = 5;
Blue = 6;
Neg = -3;
$head = 7;
X1 = 0;
X2 = 1;
sizeofPair = 104;
aggr Pair
{
	'D' 0 a;
	'b' 4 b;
	'd' 6 c;
	'u' 8 d;
	'U' 12 e;
	'D' 16 f;
	'U' 20 g;
	'V' 24 h;
	'W' 32 i;
	'f' 40 j;
	'F' 48 k;
	'a' 56 l;
	'Y' 72 fwd;
	'Y' 80 fn;
	'D' 88 en;
	'U' 92 bits1;
	'U' 96 bits2;
	'D' 100 $tail;
};

defn
Pair(addr) {
	complex Pair addr;
	print("	a	", addr.a, "\n");
	print("	b	", addr.b, "\n");
	print("	c	", addr.c, "\n");
	print("	d	", addr.d, "\n");
	print("	e	", addr.e, "\n");
	print("	f	", addr.f, "\n");
	print("	g	", addr.g, "\n");
	print("	h	", addr.h, "\n");
	print("	i	", addr.i, "\n");
	print("	j	", addr.j, "\n");
	print("	k	", addr.k, "\n");
	print("	l	", addr.l, "\n");
	print("	fwd	", addr.fwd\Y, "\n");
	print("	fn	", addr.fn\Y, "\n");
	print("	en	", addr.en, "\n");
	print("	bits1	", addr.bits1, "\n");
	print("	bits2	", addr.bits2, "\n");
	print("	$tail	", addr.$tail, "\n");
};

sizeof_1_ = 8;
aggr _1_
{
	'D' 0 x;
	'D' 4 y;
};

defn
_1_(addr) {
	complex _1_ addr;
	print("	x	", addr.x, "\n");
	print("	y	", addr.y, "\n");
};

sizeof_2_ = 8;
aggr _2_
{
	'D' 0 u;
};

defn
_2_(addr) {
	complex _2_ addr;
	print("	u	", addr.u, "\n");
};

sizeof_3_ = 8;
aggr _3_
{
	'D' 0 l;
	'F' 0 d;
};

defn
_3_(addr) {
	complex _3_ addr;
	print("	l	", addr.l, "\n");
	print("	d	", addr.d, "\n");
};

Blue = 6;
sizeofNode = 160;
aggr Node
{
	{
	'D' 0 key;
	'U' 4 pc;
	};
	'A' Node 8 next;
	'A' Node 16 pp;
	Pair 24 pair;
	{
	'D' 128 x;
	'D' 132 y;
	};
	_2_ 136 named;
	_3_ 144 un;
	'D' 152 Blue;
	'D' 156 gp;
};

defn
Node(addr) {
	complex Node addr;
	print("Lock {\n");
		Lock(addr+0);
	print("}\n");
	print("	next	", addr.next\Y, "\n");
	print("	pp	", addr.pp\Y, "\n");
	print("Pair pair {\n");
	Pair(addr.pair);
	print("}\n");
	print("_1_ {\n");
		_1_(addr+128);
	print("}\n");
	print("_2_ named {\n");
	_2_(addr.named);
	print("}\n");
	print("_3_ un {\n");
	_3_(addr.un);
	print("}\n");
	print("	Blue	", addr.Blue, "\n");
	print("	gp	", addr.gp, "\n");
};

sizeof$aggr = 8;
aggr $aggr
{
	'D' 0 $loop;
};

defn
$aggr(addr) {
	complex $aggr addr;
	print("	$loop	", addr.$loop, "\n");
};

complex Pair gp;
complex Node list;
complex Pair spair;
complex Node enode;
complex $aggr agg;
complex Node f:n;
complex Pair f:p;
complex Pair f:lp;
complex Node sn$1;
complex Node f:q;
complex Pair inner$2;
complex Pair f:blk;
complex Node ins$4;
sizeofLocal = 8;
aggr Local
{
	'D' 0 z;
};

defn
Local(addr) {
	complex Local addr;
	print("	z	", addr.z, "\n");
};

complex Local f:loc;
sizeofFwd = 16;
aggr Fwd
{
	'Y' 0 name;
	'A' Pair 8 sn;
};

defn
Fwd(addr) {
	complex Fwd addr;
	print("	name	", addr.name\Y, "\n");
	print("	sn	", addr.sn\Y, "\n");
};

complex Fwd fwdvar;
complex Fwd g:fw;
complex Pair gp;
complex Fwd sfw$6;
"#;

/// What `two.c` declares.
const TWO: &str = r#"sizeofTwo = 16;
aggr Two
{
	Lock 0 l;
	'D' 8 n;
};

defn
Two(addr) {
	complex Two addr;
	print("Lock l {\n");
	Lock(addr.l);
	print("}\n");
	print("	n	", addr.n, "\n");
};

complex Two two;
"#;

/// A directory of its own that holds the sources and a link to the compiler called `6c`.
fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-kencc-acid-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    for (name, text) in [("one.h", ONE_H), ("one.c", ONE_C), ("two.c", TWO_C)] {
        std::fs::write(dir.join(name), text).expect("the fixture can be written");
    }
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_rucc"), dir.join("6c"))
        .expect("the link can be made");
    dir
}

/// The compiler under `program`, given these flags, in the directory `dir`.
fn run(dir: &Path, program: &str, flags: &[&str]) -> Output {
    Command::new(program)
        .args(flags)
        .current_dir(dir)
        .env("INCLUDE", "")
        .output()
        .expect("the compiler is built before its own tests run")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_writes_the_acid_of_the_unit_to_the_standard_output() {
    let dir = fixture("a");
    let out = run(&dir, "./6c", &["-a", "-w", "one.c"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), format!("{LOCK}{ONE}"));
    // `-a` writes no object.
    assert!(!dir.join("one.6").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn aa_writes_only_what_the_named_file_declares() {
    let dir = fixture("aa");
    let out = run(&dir, "./6c", &["-aa", "-w", "one.c", "two.c"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), format!("{ONE}{TWO}"));
    // kencc names each file on the standard error when it is given more than one.
    assert_eq!(text(&out.stderr), "one.c:\ntwo.c:\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn n_writes_the_acid_to_a_file() {
    let dir = fixture("n");
    let out = run(&dir, "./6c", &["-an", "-w", "one.c"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty());
    let written = std::fs::read_to_string(dir.join("one.acid")).expect("-n writes one.acid");
    assert_eq!(written, format!("{LOCK}{ONE}"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn emit_acid_works_under_the_rucc_name() {
    let dir = fixture("emit");
    let rucc = env!("CARGO_BIN_EXE_rucc");
    let flags = ["--target=x86_64-plan9", "-fkencc", "-w", "--emit=acid-main", "-o", "-", "one.c"];
    let out = run(&dir, rucc, &flags);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), ONE);
    let _ = std::fs::remove_dir_all(&dir);
}
