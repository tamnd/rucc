//! A callback read from a `static const` table that an `always_inline` body was handed is inlined
//! once the body is in, as gcc does.
//!
//! The kernel's augmented rbtrees are this shape. `__rb_erase_augmented` calls
//! `augment->propagate` and `augment->copy`, and mm/interval_tree.c hands it a `static const`
//! table of two `static inline` callbacks. The calls were made direct only after the last round of
//! inlining, so every `vma_interval_tree_remove` on the munmap path called both.

use std::process::Command;

const SOURCE: &str = "\
struct node { unsigned long last, sub; struct node *l, *r, *p; };
struct cb {
	void (*propagate)(struct node *, struct node *);
	void (*copy)(struct node *, struct node *);
};
static inline unsigned long compute(struct node *n)
{
	unsigned long m = n->last;
	if (n->l && n->l->sub > m)
		m = n->l->sub;
	if (n->r && n->r->sub > m)
		m = n->r->sub;
	return m;
}
static inline void prop(struct node *n, struct node *stop)
{
	while (n != stop) {
		unsigned long m = compute(n);
		if (n->sub == m)
			break;
		n->sub = m;
		n = n->p;
	}
}
static inline void cpy(struct node *a, struct node *b) { b->sub = a->sub; }
static const struct cb cbs = { prop, cpy };
static inline __attribute__((always_inline)) void erase(struct node *n, const struct cb *c)
{
	if (n->r)
		c->copy(n, n->r);
	c->propagate(n->p, 0);
}
void f(struct node *n) { erase(n, &cbs); }
";

/// The IR the compiler produced for the source at `-O2`.
fn ir(target: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("rucc-inline-const-callbacks-{}-{target}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-O2", "--emit=ir", "-w", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn callbacks_from_a_const_table_are_inlined() {
    for target in ["i686-linux-gnu", "x86_64-linux-gnu"] {
        let text = ir(target);
        let calls = text.lines().filter(|line| calls(line)).collect::<Vec<_>>();
        assert!(calls.is_empty(), "{target}: {calls:?}\n{text}");
    }
}

/// Whether a line of IR is a call, whatever attributes it has, as in `call.nofree`.
fn calls(line: &str) -> bool {
    line.split_whitespace().any(|word| word == "call" || word.starts_with("call."))
}
