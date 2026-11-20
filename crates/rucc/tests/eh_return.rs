//! A function that leaves through `__builtin_eh_return` arrives at the handler with the stack
//! pointer it asked for and with the registers its caller kept.
//!
//! tamnd/rucc#4100. libgcc's `unwind-dw2.c` throws an exception with these builtins and no
//! assembly of its own. This is the same walk in C: `leave` saves every kept register, writes
//! over them, and leaves for `resume` with the stack pointer where `middle` would have returned
//! to. `outer` keeps five values across the call in the registers `leave` wrote over, so the sum
//! it prints is right only if the way out put them back. gcc 13.3.0 prints the same line.
//!
//! The program has no frame pointer. With one, `middle` keeps its own frame pointer and the
//! handler, which is not a landing pad, does not put it back, so gcc's build of the same program
//! does not come back either.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-eh-return-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler finished, and what it said.
fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The walk, and a `main` that prints what it came back with and the three constants.
const PROGRAM: &str = r#"
int printf(const char *, ...);

static int ran;

/* Where control arrives instead of the return from middle. It is entered by a jump with the
   stack pointer at the return address middle was called with, so its own return goes there. */
__attribute__((noinline)) long resume(void) { return 1234; }

/* Leaves for resume when asked to, and returns its answer otherwise. The asm writes over every
   register a callee keeps, so the way out to the handler has to put them back. */
__attribute__((noinline)) long leave(char *target, long go) {
  __builtin_unwind_init();
  __asm__ volatile("xorl %%ebx, %%ebx\n\txorl %%r12d, %%r12d\n\txorl %%r13d, %%r13d\n\t"
                   "xorl %%r14d, %%r14d\n\txorl %%r15d, %%r15d" ::: "rbx", "r12", "r13", "r14", "r15");
  if (!go)
    return 77;
  long offset = target - (char *)__builtin_dwarf_cfa();
  __builtin_eh_return(offset, (void *)resume);
}

/* Saves nothing of its own, so the registers leave saved are outer's. */
__attribute__((noinline)) long middle(long go) {
  long got = leave((char *)__builtin_dwarf_cfa() - sizeof(void *), go);
  ran = 1;
  return got;
}

__attribute__((noinline)) long outer(long a, long b, long c, long d, long e) {
  long x = a * 3, y = b * 5, z = c * 7, w = d * 11, v = e * 13;
  long got = middle(1);
  return got + x + y + z + w + v + a + b + c + d + e;
}

int main(void) {
  unsigned char sizes[64];
  for (int i = 0; i < 64; i++) sizes[i] = 0xff;
  __builtin_init_dwarf_reg_size_table(sizes);
  long plain = leave(0, 0);
  long far = outer(1, 2, 3, 4, 5);
  printf("%ld %ld %d %u %d %d %d %d %d\n", plain, far, ran, __builtin_dwarf_sp_column(),
         __builtin_eh_return_data_regno(0), __builtin_eh_return_data_regno(1),
         __builtin_eh_return_data_regno(2), sizes[7], sizes[17]);
  return 0;
}
"#;

/// What gcc 13.3.0 prints for the program at every level: the plain return, the sum `outer`
/// returns through the handler, that `middle` never went on past its call, the stack pointer
/// column, the three data registers and two bytes of the size table.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PRINTED: &str = "77 1392 0 7 0 1 -1 8 255\n";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_handler_two_frames_up_is_reached_with_its_registers() {
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for level in ["-O0", "-O1", "-O2", "-O3", "-Os"] {
        let (ok, said) = run(&dir, &[level, "-fomit-frame-pointer", "a.c", "-o", "prog"]);
        assert!(ok, "{level}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert!(out.status.success(), "{level}: the program failed");
        assert_eq!(String::from_utf8_lossy(&out.stdout), PRINTED, "{level}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// On any host, the way out of `leave` is the jump through rcx and the plain return is still
/// there, and what `middle` returns is not folded to what the plain return gives.
#[test]
fn the_way_out_to_the_handler_is_a_jump_through_rcx() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let target = "--target=x86_64-unknown-linux-gnu";
    let (ok, said) = run(&dir, &[target, "-O2", "-fomit-frame-pointer", "-S", "a.c", "-o", "a.s"]);
    assert!(ok, "{said}");
    let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
    let start = asm.find("\nleave:").expect("the function is there");
    let end = asm[start..].find("\t.size").map_or(asm.len(), |at| start + at);
    let body = &asm[start..end];
    assert!(body.contains("\tjmp\t*%rcx\n"), "{body}");
    assert!(body.contains("\tret\n"), "{body}");
    assert!(body.contains("\tpushq\t%rax\n") && body.contains("\tpushq\t%rdx\n"), "{body}");
    // `leave` has one return, of 77, and the other way out is not a return at all. What `middle`
    // hands back is still what the call gave it rather than that 77.
    let start = asm.find("\nmiddle:").expect("the function is there");
    let end = asm[start..].find("\t.size").map_or(asm.len(), |at| start + at);
    let body = &asm[start..end];
    assert!(body.contains("call\tleave"), "{body}");
    assert!(!body.contains("$77"), "{body}");
    let _ = std::fs::remove_dir_all(&dir);
}
