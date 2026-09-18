//! Inline assembly end to end.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.2.
//!
//! An `asm` whose template has no instructions in it is most of the inline assembly in a test
//! suite, and it is not an accident of one. A program that wants a value computed where it stands,
//! or a loop nothing may touch, writes `asm volatile ("" : : : "memory")`, and what it is asking
//! for is the barrier and the places the operands share rather than any instruction. A template
//! that does name an instruction gets that instruction, looked up in the same description of the
//! machine the listing is written from, so what is checked here is that the name a program wrote
//! and the name the listing carries are the same one.
//!
//! The unit tests in `rucc-ir` cover reading a constraint list back, and the ones in `rucc-codegen`
//! cover what each shape lowers to. What is left is the trip itself, which is only visible from the
//! outside, so this runs the compiler over C and reads the listing.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the listing this compares is
/// the same listing on every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-inline-asm-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler says about that source, and whether it finished.
fn run(what: &str, source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.success(), text, said)
}

/// The assembly the compiler writes for that source.
fn asm(what: &str, source: &str) -> String {
    let (ok, text, said) = run(what, source);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    text
}

/// The body of one function of the listing, which is what each of these is about.
fn body(text: &str, name: &str) -> String {
    let start = text.find(&format!("\n{name}:\n")).expect("the function is in the listing");
    let rest = &text[start + 1..];
    let end = rest.find("\t.size").expect("every function is followed by its size");
    rest[..end].to_string()
}

#[test]
fn a_barrier_is_no_instructions_at_all() {
    // The barrier was spent on the optimizer, which has finished by the time anything is written,
    // so what is left of one is nothing.
    let with = asm("barrier", "int f(int x) { asm volatile (\"\" : : : \"memory\"); return x; }\n");
    let without = asm("plain", "int f(int x) { return x; }\n");
    assert_eq!(body(&with, "f"), body(&without, "f"));
}

#[test]
fn a_value_passed_through_a_tied_pair_comes_back_unchanged() {
    // `asm ("" : "=r" (x) : "0" (x))` is how a program stops the optimizer following a value
    // without changing it. The two share a place and the template writes nothing over it, so what
    // comes out is what went in, and the function is the identity it reads as.
    let text = asm("tied", "int f(int x) { asm (\"\" : \"=r\" (x) : \"0\" (x)); return x; }\n");
    let body = body(&text, "f");
    assert!(body.contains("%rdi"), "the argument was never read:\n{body}");
    assert!(body.contains("ret"), "{body}");
}

#[test]
fn an_output_written_plus_says_the_same_thing_in_one_operand() {
    let text = asm("plus", "int f(int x) { asm (\"\" : \"+r\" (x)); return x; }\n");
    let body = body(&text, "f");
    assert!(body.contains("%rdi"), "the argument was never read:\n{body}");
}

#[test]
fn an_operand_in_memory_is_the_object_it_names() {
    // An `"m"` operand is handed over as an address, so the object has to be somewhere with one.
    // Nothing is written through it here, because the template writes nothing.
    let text = asm("memory", "int f(int x) { asm (\"\" : \"=m\" (x) : \"m\" (x)); return x; }\n");
    let body = body(&text, "f");
    assert!(body.contains("(%rsp)") || body.contains("(%rax)"), "{body}");
}

#[test]
fn a_template_with_an_instruction_in_it_is_that_instruction() {
    let text = asm("real", "int f(int x) { asm volatile (\"pause\"); return x; }\n");
    let body = body(&text, "f");
    assert!(body.contains("pause"), "the template never reached the listing:\n{body}");
}

#[test]
fn a_template_that_reads_a_segment_is_the_load_it_names() {
    // What rpmalloc writes to find the block its own thread owns. The instruction it asks for is
    // the one a thread local variable already compiles to, reached this time through the template.
    let source = "long f(void) { long t; asm (\"movq %%fs:0, %0\" : \"=r\" (t)); return t; }\n";
    let text = asm("segment", source);
    let body = body(&text, "f");
    assert!(body.contains("%fs:0"), "the segment never reached the listing:\n{body}");
}

#[test]
fn a_two_address_instruction_is_read_as_the_operand_it_is_told_to_overwrite() {
    // AT&T writes an addition with two arguments and this machine describes it with three, the
    // third being the destination before the instruction ran. The tie between them is in the
    // description, so the template says everything it has to and `"+r"` is what makes the operand
    // one the statement is willing to have written over.
    let source =
        "long f(long x, long y) { asm (\"addq %1, %0\" : \"+r\" (x) : \"r\" (y)); return x; }\n";
    let text = asm("two-address", source);
    let body = body(&text, "f");
    assert!(body.contains("addq"), "the template never reached the listing:\n{body}");
}

#[test]
fn a_width_written_on_an_operand_is_the_width_the_instruction_is_read_at() {
    // What libgmp writes throughout `longlong.h`, which every file of that library includes. The
    // header is shared with the thirty two bit target, where a limb is narrower and the same line
    // still has to say sixty four bits, so the width is on the operand rather than on the mnemonic.
    // Here the mnemonic carries no suffix at all and the letter in front of the number is the only
    // thing saying how wide the addition is.
    let source =
        "long f(long x, long y) { asm (\"add %q1, %q0\" : \"+r\" (x) : \"r\" (y)); return x; }\n";
    let text = asm("modifier", source);
    let body = body(&text, "f");
    assert!(body.contains("addq"), "the width on the operand was not read:\n{body}");
}

#[test]
fn a_width_that_disagrees_with_what_it_is_written_on_says_so() {
    // Two ways of disagreeing and both are refused rather than one half being believed over the
    // other. A quadword add into half a register is not an instruction, and a quadword add of an
    // `int` is an instruction reaching a register half of which nothing defined.
    let mnemonic =
        "long f(long x, long y) { asm (\"addq %1, %k0\" : \"+r\" (x) : \"r\" (y)); return x; }\n";
    let (ok, _, said) = run("modifier-mnemonic", mnemonic);
    assert!(!ok, "a width that contradicts the mnemonic was accepted");
    assert!(said.contains("which nothing here assembles"), "{said}");
    let ty = "int f(int x, int y) { asm (\"add %q1, %q0\" : \"+r\" (x) : \"r\" (y)); return x; }\n";
    let (ok, _, said) = run("modifier-type", ty);
    assert!(!ok, "a width that contradicts the type of the operand was accepted");
    assert!(said.contains("has an operand this cannot place"), "{said}");
}

#[test]
fn a_template_this_cannot_place_still_says_what_is_missing() {
    // Refused rather than dropped. A template nothing here can place is a program this compiler
    // cannot build, and a template quietly left out is a program that builds and does the wrong
    // thing. The same addition as above on an output written `=`, which says the assembly writes
    // the operand and never reads it, while the instruction reads it before it writes it.
    let source = "long f(long x) { asm (\"addq %1, %0\" : \"=r\" (x) : \"r\" (x)); return x; }\n";
    let (ok, _, said) = run("write-only", source);
    assert!(!ok, "an operand read where the statement said it is only written was accepted");
    assert!(said.contains("has an operand this cannot place"), "{said}");
}

#[test]
fn a_comparison_and_a_conditional_move_keep_a_select_branchless() {
    // What zstd writes in `ZSTD_selectAddr` so that a bounds check does not become a branch the
    // processor has to guess at. Neither mnemonic carries its suffix, the pair is two instructions
    // that have to stay in that order, and the move overwrites the operand written `+`.
    let source = "\
char *f(unsigned a, unsigned b, char *p, char *q) {
  asm (\"cmp %1, %2\\n\\tcmova %3, %0\" : \"+r\" (p) : \"r\" (a), \"r\" (b), \"r\" (q));
  return p;
}
";
    let text = asm("cmov", source);
    let body = body(&text, "f");
    assert!(body.contains("cmpl"), "the comparison was read at the wrong width:\n{body}");
    assert!(body.contains("cmovaq"), "the move was read at the wrong width:\n{body}");
    let compare = body.find("cmpl").expect("a comparison");
    let move_ = body.find("cmovaq").expect("a move");
    assert!(compare < move_, "the move was put in front of the comparison it reads:\n{body}");
}

#[test]
fn an_alignment_in_a_template_reaches_the_listing_as_the_directive_it_asks_for() {
    // What zstd writes immediately in front of the match loop of `ZSTD_compressBlock_lazy_generic`,
    // in `lib/compress/zstd_lazy.c`. The loop is the hot one of the whole compressor and the
    // program is asking that it start on a boundary, which is a thing to say about where the next
    // instruction goes rather than an instruction of its own.
    let source = "\
int f(int n) {
  int total = 0;
  for (int i = 0; i < n; i++) {
    asm volatile (\".p2align 5\");
    total += i;
  }
  return total;
}
";
    let text = asm("align", source);
    let body = body(&text, "f");
    assert!(body.contains(".p2align\t5"), "the alignment never reached the listing:\n{body}");
}

#[test]
fn an_alignment_this_cannot_promise_says_so_rather_than_dropping_it() {
    // A second argument is a fill byte or a number of bytes to stop after, and both are things this
    // does not do. Quietly writing the boundary without the rest of what was asked for is a program
    // that builds and is not the one that was written.
    let source = "void f(void) { asm volatile (\".p2align 4, 0x90, 8\"); }\n";
    let (ok, _, said) = run("align-fill", source);
    assert!(!ok, "an alignment with more in it than a boundary was accepted");
    assert!(said.contains("which nothing here assembles"), "{said}");
}

#[test]
fn an_asm_goto_says_what_is_missing_too() {
    let source = "int f(int x) { asm goto (\"\" : : : : away); return x; away: return 0; }\n";
    let (ok, _, said) = run("goto", source);
    assert!(!ok, "an `asm goto` was accepted");
    assert!(said.contains("jumps to a label"), "{said}");
}
