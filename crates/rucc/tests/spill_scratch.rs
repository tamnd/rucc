//! A function that spills is given `r11` and holds only `r10` back to read its spilled values into.
//!
//! Issue 1994. x86-64 holds two registers back for the rewrite, and hands both out to a function
//! that never reads a value off the stack. A function that does read one lost both, and the hot
//! loop of Postgres' tuple deforming read three values off the stack on every turn while `r11` sat
//! idle. `spill_scratch.c` is that loop cut down from REL_18_6. The unit tests in `rucc-regalloc`
//! cover when one register is enough. This reads the listing, and on x86-64 Linux runs the loop
//! against tuples built to a known layout.

use std::process::Command;

/// The fixture, which is the cut-down deforming loop.
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/spill_scratch.c");

/// The lines of one function in the listing, from its label to the end of its unwind information.
fn body<'a>(asm: &'a str, function: &str) -> Vec<&'a str> {
    let label = format!("{function}:");
    asm.lines()
        .skip_while(|line| line.trim() != label)
        .take_while(|line| !line.contains(".cfi_endproc"))
        .map(str::trim)
        .collect()
}

#[test]
fn the_deforming_loop_is_given_r11() {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("--target=x86_64-unknown-linux-gnu")
        .args(["-S", "-o", "-", "-O2"])
        .arg(FIXTURE)
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let asm = String::from_utf8(out.stdout).expect("what the compiler writes is text");
    let lines = body(&asm, "tts_buffer_heap_getsomeattrs");
    assert!(lines.len() > 1, "no tts_buffer_heap_getsomeattrs in:\n{asm}");
    let listing = lines.join("\n");
    assert!(listing.contains("%r11"), "r11 is never used in:\n{listing}");
}

/// A program that builds tuples of every kind of column the loop reads, deforms each one twice,
/// once all at once and once in two parts, and says which check failed in its exit status.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
const CHECK: &str = r"
#include <string.h>

typedef unsigned long Datum;
typedef struct {
  int attcacheoff;
  short attlen;
  _Bool attbyval, attispackable, atthasmissing, attisdropped, attgenerated;
  char attnullability;
  unsigned char attalignby;
} Att;
typedef struct {
  int natts;
  unsigned tdtypeid;
  int tdtypmod;
  int tdrefcount;
  void *constr;
  Att attrs[8];
} Desc;
typedef struct {
  unsigned t_len;
  char t_self[6];
  unsigned t_tableOid;
  unsigned char *t_data;
} Tuple;
typedef struct {
  int type;
  unsigned short tts_flags;
  short tts_nvalid;
  const void *tts_ops;
  Desc *desc;
  Datum *values;
  _Bool *isnull;
  void *mcxt;
  char tid[6];
  unsigned tableOid;
  Tuple *tuple;
  unsigned off;
  Tuple tupdata;
  int buffer;
} Slot;

void tts_buffer_heap_getsomeattrs(Slot *slot, int natts);

static unsigned long state = 88172645463325252ul;

static unsigned pick(unsigned n) {
  state ^= state << 13;
  state ^= state >> 7;
  state ^= state << 17;
  return (unsigned) (state % n);
}

static unsigned long align(unsigned long at, unsigned by) {
  return (at + by - 1) & ~(unsigned long) (by - 1);
}

static unsigned char page[8192] __attribute__((aligned(8)));

int main(void) {
  for (int round = 0; round < 20000; round++) {
    Desc desc;
    Datum want[8];
    _Bool null[8];
    memset(&desc, 0, sizeof desc);
    memset(page, 0, sizeof page);
    int natts = 1 + (int) pick(8);
    desc.natts = natts;
    _Bool hasnulls = pick(2);
    unsigned char *tup = page;
    unsigned char *tp = tup + 24;
    unsigned long off = 0;
    for (int i = 0; i < natts; i++) {
      Att *a = &desc.attrs[i];
      a->attcacheoff = -1;
      null[i] = hasnulls && pick(3) == 0;
      switch (pick(6)) {
      case 0: a->attlen = 1; a->attbyval = 1; a->attalignby = 1; break;
      case 1: a->attlen = 2; a->attbyval = 1; a->attalignby = 2; break;
      case 2: a->attlen = 4; a->attbyval = 1; a->attalignby = 4; break;
      case 3: a->attlen = 8; a->attbyval = 1; a->attalignby = 8; break;
      case 4: a->attlen = -1; a->attalignby = 4; break;
      default: a->attlen = -2; a->attalignby = 1; break;
      }
      if (null[i]) {
        want[i] = 0;
        continue;
      }
      if (a->attlen > 0) {
        off = align(off, a->attalignby);
        unsigned long v = (unsigned long) pick(1u << 31) << 32 | pick(1u << 31);
        v |= (unsigned long) pick(2) << 63;
        memcpy(tp + off, &v, (size_t) a->attlen);
        switch (a->attlen) {
        case 1: want[i] = (Datum) (long) (signed char) v; break;
        case 2: want[i] = (Datum) (long) (short) v; break;
        case 4: want[i] = (Datum) (long) (int) v; break;
        default: want[i] = v; break;
        }
        off += (unsigned long) a->attlen;
      } else if (a->attlen == -1) {
        unsigned len = 1 + pick(200);
        if (len < 127 && pick(2)) {
          tp[off] = (unsigned char) ((len + 1) << 1 | 1);
          memset(tp + off + 1, 'a', len);
          want[i] = (Datum) (tp + off);
          off += len + 1;
        } else {
          off = align(off, 4);
          unsigned word = (len + 4) << 2;
          memcpy(tp + off, &word, 4);
          memset(tp + off + 4, 'b', len);
          want[i] = (Datum) (tp + off);
          off += len + 4;
        }
      } else {
        unsigned len = pick(20);
        memset(tp + off, 'c', len);
        tp[off + len] = 0;
        want[i] = (Datum) (tp + off);
        off += len + 1;
      }
    }
    unsigned short count = (unsigned short) natts, mask = hasnulls;
    memcpy(tup + 18, &count, 2);
    memcpy(tup + 20, &mask, 2);
    tup[22] = 24;
    for (int i = 0; i < natts; i++)
      if (!null[i])
        tup[23] |= (unsigned char) (1 << i);

    Datum values[8];
    _Bool isnull[8];
    Tuple tuple;
    Slot slot;
    memset(&tuple, 0, sizeof tuple);
    memset(&slot, 0, sizeof slot);
    tuple.t_data = tup;
    slot.desc = &desc;
    slot.values = values;
    slot.isnull = isnull;
    slot.tuple = &tuple;
    int first = (int) pick((unsigned) natts + 1);
    for (int pass = 0; pass < 2; pass++) {
      slot.tts_nvalid = 0;
      slot.tts_flags = 0;
      slot.off = 0;
      memset(values, 0x55, sizeof values);
      memset(isnull, 1, sizeof isnull);
      if (pass == 1 && first > 0)
        tts_buffer_heap_getsomeattrs(&slot, first);
      tts_buffer_heap_getsomeattrs(&slot, natts);
      if (slot.tts_nvalid != natts)
        return 1;
      for (int i = 0; i < natts; i++) {
        if (isnull[i] != null[i])
          return 2;
        if (values[i] != want[i])
          return 3;
      }
    }
  }
  return 0;
}
";

/// A directory of its own for the program, so that two runs at once do not share files.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-spill-scratch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn the_deforming_loop_reads_back_what_was_written() {
    let dir = scratch();
    std::fs::write(dir.join("check.c"), CHECK).expect("the program can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .args(["-O2", "check.c"])
        .arg(FIXTURE)
        .args(["-o", "check"])
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let ran = Command::new(dir.join("check")).status().expect("what was linked can be run");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(ran.code(), Some(0), "a column came back different from what was written");
}
