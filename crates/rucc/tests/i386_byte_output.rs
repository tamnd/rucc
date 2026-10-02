//! A `q` output on i386 when the statement takes every register with a low byte. The kernel's
//! `__arch_try_cmpxchg64` hands `cmpxchg8b` `edx:eax` and `ecx:ebx` and reads the answer back
//! with `CC_OUT(e)`, which is a `"=q"` written with `sete`. Only those four registers have a low
//! byte on i386, and the output used to land in `esi`, where `sete %sil` is not an instruction
//! the machine has. gcc puts it in `ebx` or `ecx`, which an output not written early may share
//! with an input, and so does this.
//!
//! A `q` output whose value the allocator keeps on the stack used to be written through the
//! scratch register too, which is `esi`. fs/buffer.c reads `buffer_uptodate` that way in
//! `fsync_buffers_list`. Such an output is now given a register with a low byte whenever one is
//! free.

use std::process::Command;

const SOURCE: &str = "\
unsigned long long v;
int f(unsigned long long o, unsigned lo, unsigned hi) {
  int ret;
  asm volatile(\"lock cmpxchg8b %[ptr]\" : \"=@ccz\"(ret), [ptr] \"+m\"(v), \"+A\"(o)
               : \"b\"(lo), \"c\"(hi) : \"memory\");
  return ret + lo + hi;
}
int g(unsigned long long *p, unsigned long long o, unsigned long long n) {
  unsigned char ok;
  asm volatile(\"lock; cmpxchg8b %1\\n\\tsete %0\" : \"=q\"(ok), \"+m\"(*p), \"+A\"(o)
               : \"b\"((unsigned)n), \"c\"((unsigned)(n >> 32)) : \"memory\");
  return ok;
}
";

/// Four inputs that only ask for a register, which fill the four with a low byte before the
/// output is placed, as a comparison of four values does.
const FOUR: &str = "\
_Bool t(long a, long b, long c, long d, long *p) {
  _Bool r;
  asm(\"cmpl %1,%2\" : \"=@cce\"(r) : \"r\"(a), \"r\"(b), \"r\"(c), \"r\"(d));
  *p = a + b + c + d;
  return r;
}
";

const SPILLED: &str = "\
struct bh { unsigned long state; int count; struct bh *next; void *map; };
static inline _Bool tb(long nr, const volatile unsigned long *addr) {
  _Bool oldbit;
  asm volatile(\"testb %2,%1\" : \"=@ccnz\" (oldbit) : \"m\" (((volatile const char *)addr)[nr >> 3]), \"i\" (1 << (nr & 7)) : \"memory\");
  return oldbit;
}
void lock(void *); void unlock(void *); void resched(void); void wait(struct bh *); void put(struct bh *);
int f(struct bh **list, void *l) {
  int err = 0;
  while (*list) {
    struct bh *bh = *list;
    void *map = bh->map;
    *list = bh->next;
    if (tb(1, &bh->state)) { bh->next = *list; bh->map = map; }
    unlock(l);
    resched();
    if (tb(2, &bh->state)) wait(bh);
    if (!tb(0, &bh->state)) err = -5;
    if (bh) { if (bh->count) __atomic_fetch_sub(&bh->count, 1, 0); else put(bh); }
    lock(l);
  }
  return err;
}
";

fn listing(level: &str) -> String {
    listing_of(SOURCE, level)
}

fn listing_of(source: &str, level: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "rucc-i386-byte-{}-{}{}",
        std::process::id(),
        source.len(),
        level.replace(' ', "")
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-fno-pic", "-S", "-o", "-"])
        .args(level.split_whitespace())
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn the_byte_output_shares_a_register_with_an_input() {
    for level in ["-O0", "-O2"] {
        let text = listing(level);
        for byteless in ["%sil", "%dil", "%bpl", "%spl"] {
            assert!(!text.contains(byteless), "{level}: {byteless}:\n{text}");
        }
        let set = |name: &str| {
            text.contains(&format!("{name} %bl")) || text.contains(&format!("{name} %cl"))
        };
        assert!(set("setz") && set("sete"), "{level}:\n{text}");
    }
}

#[test]
fn the_byte_output_shares_with_an_input_when_inputs_fill_the_four() {
    for level in ["-O0", "-O2"] {
        for regparm in ["-mregparm=0", "-mregparm=3"] {
            let text = listing_of(FOUR, &format!("{level} {regparm}"));
            for byteless in ["%sil", "%dil", "%bpl", "%spl"] {
                assert!(!text.contains(byteless), "{level} {regparm}: {byteless}:\n{text}");
            }
        }
    }
}

#[test]
fn a_byte_output_whose_value_is_kept_on_the_stack_is_written_to_a_byte_register() {
    for level in ["-O1", "-O2"] {
        let text = listing_of(SPILLED, &format!("{level} -mregparm=3"));
        for byteless in ["%sil", "%dil", "%bpl", "%spl"] {
            assert!(!text.contains(byteless), "{level}: {byteless}:\n{text}");
        }
    }
}
