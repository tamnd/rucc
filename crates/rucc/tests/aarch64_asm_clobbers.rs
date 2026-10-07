//! What an extended `asm` statement on AArch64 takes away from the register allocator, which is
//! what its clobber list names and what its text spells and nothing more, as gcc reads it. A
//! template that calls out, and basic assembly, still take every register a call may write. A
//! register the text only reads, as a store, a compare or a branch reads it, is not taken away,
//! so the kernel's `crash_setup_regs`, which stores all thirty one, still leaves room for its
//! operands.
//!
//! Before this every AArch64 template was taken to be a call, so each operand went in a register
//! a call keeps and the kernel's atomics saved and restored two of them around one `ldadd`.

use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET: &str = "--target=aarch64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-a64-clobbers-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// The `-O2` listing of a source that has to compile without a word.
fn listing(what: &str, source: &str) -> String {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([TARGET, "-S", "-O2", "-o", "-", "a.c"])
        .current_dir(Path::new(&dir))
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && err.is_empty(), "{err}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The instructions of one function, without its labels and directives.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.starts_with("\t.size"))
        .filter(|line| line.starts_with('\t') && !line.starts_with("\t."))
        .map(|line| line.trim().to_string())
        .collect()
}

#[test]
fn an_atomic_in_a_template_saves_nothing() {
    let text = listing(
        "ldadd",
        "long add(long *v, long i) {
  long r;
  asm volatile(\".arch_extension lse\\n\\tldaddal %[i], %[r], %[v]\"
               : [v] \"+Q\"(*v), [r] \"=r\"(r) : [i] \"r\"(i) : \"memory\");
  return r;
}
",
    );
    let add = body(&text, "add");
    assert!(!add.iter().any(|line| line.starts_with("stp")), "{text}");
    assert!(!text.contains("x19"), "{text}");
}

#[test]
fn a_value_held_across_a_template_stays_where_it_was() {
    let text = listing(
        "held",
        "long held(long a, long b) { asm volatile(\"dmb ish\" ::: \"memory\"); return a + b; }\n",
    );
    assert_eq!(body(&text, "held"), ["dmb ish", "add x0, x0, x1", "ret"], "{text}");
}

#[test]
fn a_register_the_text_spells_or_the_list_names_is_not_held_across_it() {
    let text = listing(
        "spelled",
        "long spelled(long a, long b) { asm volatile(\"mov x1, #1\" ::: \"memory\"); return a + b; }
long named(long a, long b) { asm volatile(\"\" ::: \"x0\", \"x1\"); return a + b; }
long operand(long a, long b) { long r; asm(\"add %x0, %x1, #1\" : \"=r\"(r) : \"r\"(a)); return r + b; }
",
    );
    let spelled = body(&text, "spelled");
    assert_eq!(spelled.iter().filter(|line| line.contains("x1")).count(), 2, "{text}");
    assert!(spelled.contains(&"mov x1, #1".to_string()), "{text}");
    assert!(!spelled.contains(&"add x0, x0, x1".to_string()), "{text}");
    let named = body(&text, "named");
    assert!(!named.contains(&"add x0, x0, x1".to_string()), "{text}");
    // `%x0` and `%x1` are operands, so nothing is taken away and nothing is saved.
    assert!(!body(&text, "operand").iter().any(|line| line.starts_with("stp")), "{text}");
}

#[test]
fn a_call_and_basic_assembly_take_every_register_a_call_may_write() {
    let text = listing(
        "calls",
        "long calls(long a, long b) { asm volatile(\"bl foo\" ::: \"memory\"); return a + b; }
long basic(long a, long b) { asm volatile(\"nop\"); return a + b; }
",
    );
    for name in ["calls", "basic"] {
        assert!(body(&text, name)[0].starts_with("stp x19, x20"), "{name}:\n{text}");
    }
}

#[test]
fn a_register_the_text_only_reads_is_still_held_across_it() {
    let text = listing(
        "reads",
        "long stored(long a, long b, long *p) { asm volatile(\"str x1, [%0]\" :: \"r\"(p) : \"memory\"); return a + b; }
long compared(long a, long b) { asm volatile(\"cmp x1, #0\" ::: \"cc\"); return a + b; }
long back(long a, long b, long *p) { asm volatile(\"str x3, [x1, #8]!\" :: \"r\"(p) : \"memory\"); return a + b; }
long status(long a, long b, long *p) { asm volatile(\"stxr w1, x3, [%0]\" :: \"r\"(p) : \"memory\"); return a + b; }
",
    );
    for name in ["stored", "compared"] {
        assert_eq!(body(&text, name)[1..], ["add x0, x0, x1", "ret"], "{name}:\n{text}");
    }
    // A base written back and the status of a store exclusive are written.
    for name in ["back", "status"] {
        assert!(!body(&text, name).contains(&"add x0, x0, x1".to_string()), "{name}:\n{text}");
    }
}

#[test]
fn a_template_that_stores_every_register_still_has_room_for_its_operands() {
    // The kernel's `crash_setup_regs` in arch/arm64/include/asm/kexec.h, cut down.
    let mut stores = String::new();
    for pair in 0..15 {
        stores.push_str(&format!("stp x{}, x{}, [%2, #16 * {pair}]\\n", 2 * pair, 2 * pair + 1));
    }
    let text = listing(
        "crash",
        &format!(
            "struct regs {{ unsigned long r[34]; }} saved;
void crash(void) {{
  unsigned long a, b;
  asm volatile(\"{stores}mov %0, sp\\nstp x30, %0, [%2, #16 * 15]\\nadr %1, 1f\\n1:\\nstp %1, %0, [%2, #16 * 16]\"
               : \"=&r\"(a), \"=&r\"(b) : \"r\"(&saved) : \"memory\");
}}
"
        ),
    );
    assert!(body(&text, "crash").iter().any(|line| line.starts_with("stp x28, x29")), "{text}");
}
