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
    // `int` reads a register half of which nothing defined, which the read of `%q1` says and the
    // read half of `%q0` says again.
    let mnemonic =
        "long f(long x, long y) { asm (\"addq %1, %k0\" : \"+r\" (x) : \"r\" (y)); return x; }\n";
    let (ok, _, said) = run("modifier-mnemonic", mnemonic);
    assert!(!ok, "a width that contradicts the mnemonic was accepted");
    assert!(said.contains("which nothing here assembles"), "{said}");
    let ty = "int f(int x, int y) { asm (\"add %q1, %q0\" : \"+r\" (x) : \"r\" (y)); return x; }\n";
    let (ok, _, said) = run("modifier-type", ty);
    assert!(!ok, "a width that contradicts the type of the operand was accepted");
    assert!(said.contains("has an operand this cannot place"), "{said}");
    // And the narrow half of the same question, which is an instruction that writes a third of the
    // object it was given and leaves the rest holding whatever was there. gcc writes that one and
    // the program it writes it for is relying on what the machine does to the top of a register
    // rather than on anything it said, so this refuses it until something real asks for it.
    let part = "long f(long x) { long n; asm (\"bsf %k1, %k0\" : \"=r\" (n) : \"r\" (x)); \
                return n; }\n";
    let (ok, _, said) = run("modifier-narrow", part);
    assert!(!ok, "an instruction filling part of its output was accepted");
    assert!(said.contains("has an operand this cannot place"), "{said}");
}

#[test]
fn an_operand_written_by_more_of_the_register_than_its_type_fills_is_the_low_part_of_it() {
    // `count_trailing_zeros` out of libgmp's `longlong.h`, written the way `divis.c` reaches it
    // rather than the way the header's own comment shows it. The count goes into an `unsigned`,
    // because a count of the bits of a limb cannot exceed sixty four, and the template still spells
    // the destination `%q0` because the same line is read on the target where a limb is a `long`.
    // So the instruction is a quadword one, the object in the register is the low half of what it
    // wrote, and both of those are exactly what the program asked for.
    let source = "unsigned f(unsigned long x) { unsigned n; \
                  asm (\"rep;bsf\\t%1, %q0\" : \"=r\" (n) : \"rm\" (x)); return n; }\n";
    let body = body(&asm("modifier-wide", source), "f");
    assert!(body.contains("tzcntq"), "the width on the operand was not read:\n{body}");
    assert!(!body.contains("tzcntl"), "the type was believed over the template:\n{body}");
}

#[test]
fn a_distance_taken_from_a_constant_operand_is_written_into_the_instruction() {
    // The step libgmp walks a limb at a time, out of `MPN_INCR_U` in `gmp-impl.h`. The size of a
    // limb is the third operand rather than a number in the line, because the same line is read on
    // the target where a limb is four bytes, and `%c` is how a template says that the operand is a
    // constant and goes where a distance goes rather than where an immediate goes.
    let source = "long *f(long *p) { long *q; \
                  asm (\"lea %c2(%1), %0\" : \"=r\" (q) : \"r\" (p), \"n\" (sizeof(long))); \
                  return q; }\n";
    let body = body(&asm("displacement-operand", source), "f");
    assert!(body.contains("leaq"), "the template never reached the listing:\n{body}");
    assert!(body.contains("8("), "the distance never reached the address:\n{body}");
    assert!(!body.contains("$8"), "the constant was put in a register as well:\n{body}");
}

#[test]
fn a_repeat_prefix_in_front_of_a_bit_search_is_the_count_and_not_the_search() {
    // The other two templates `longlong.h` writes, which are how libgmp counts the zeroes at either
    // end of a limb. The prefix is not a decoration here and the library's own comment beside the
    // line says so: it is `lzcnt` spelled the way an assembler from before `lzcnt` existed would
    // take it. A search answers where the highest set bit is and a count answers how many places
    // are above it, so reading the prefixed line as the bare one would be a program that does
    // something other than what it says rather than a program that is a little slower.
    let leading = "long f(long x) { long n; asm (\"rep;bsr\\t%1, %q0\" : \"=r\" (n) : \"rm\" (x)); \
                   return n; }\n";
    let trailing = "long f(long x) { long n; asm (\"rep;bsf\\t%1, %q0\" : \"=r\" (n) : \"rm\" (x)); \
                    return n; }\n";
    // And the bare line, which is what the same header writes on a target the library was not told
    // the count exists on. It stays the search it is written as.
    let search = "long f(long x) { long n; asm (\"bsr\\t%1,%0\" : \"=r\" (n) : \"rm\" (x)); \
                  return n; }\n";
    let counted = body(&asm("count-leading", leading), "f");
    let below = body(&asm("count-trailing", trailing), "f");
    let found = body(&asm("search", search), "f");
    assert!(counted.contains("lzcntq"), "the prefix was dropped:\n{counted}");
    assert!(below.contains("tzcntq"), "the prefix was dropped:\n{below}");
    assert!(found.contains("bsrq"), "the search was not read:\n{found}");
    assert!(!found.contains("lzcnt"), "a bare search became a count:\n{found}");
}

#[test]
fn a_byte_reversal_in_a_template_is_the_one_instruction_that_does_it() {
    // What libgmp writes in `gmp-impl.h` to put a limb the other way round. Nothing in this compiler
    // selects the instruction, because a byte reversal is built out of shifts and masks so that
    // every target gives the same answer, so the only way to reach it is to name it.
    let source = "long f(long x) { asm (\"bswap %q0\" : \"+r\" (x)); return x; }\n";
    let body = body(&asm("bswap", source), "f");
    assert!(body.contains("bswapq"), "the reversal never reached the listing:\n{body}");
    assert!(!body.contains("shrq"), "the template was built out of shifts instead:\n{body}");
}

#[test]
fn a_multiply_that_keeps_both_halves_reads_the_operand_a_number_tied_to_its_output() {
    // `umul_ppmm` out of libgmp's `longlong.h`, which is how the library multiplies two limbs and
    // keeps all hundred and twenty eight bits of the answer. Written exactly as the header writes
    // it, matching constraint and all: the low half comes back in `rax`, the high half in `rdx`,
    // and one of the multiplicands has to be in `rax` on the way in, which the program says by
    // tying it to the output that is already there rather than by naming the register twice.
    //
    // So the instruction reads a register whose text names nothing, and the thing that says what is
    // in it is a constraint on one operand and a number on another. Reading only the letter would
    // find an output with no value to read and give up, and what a template gets when nothing is
    // found is a register of its own with a zero put in it, which is a library that multiplies
    // every pair of limbs to nothing.
    let source = "\
unsigned long f(unsigned long a, unsigned long b, unsigned long *hi) {
  unsigned long low, high;
  asm (\"mulq %3\" : \"=a\" (low), \"=d\" (high) : \"%0\" (a), \"rm\" (b));
  *hi = high;
  return low;
}
";
    let body = body(&asm("umul-ppmm", source), "f");
    assert!(body.contains("mulq"), "the multiply never reached the listing:\n{body}");
    assert!(!body.contains("imulq"), "the unsigned multiply became the signed one:\n{body}");
    assert!(!body.contains("$0"), "a multiplicand was zeroed rather than read:\n{body}");
}

#[test]
fn a_division_of_a_pair_of_registers_reads_both_halves_the_program_filled() {
    // `udiv_qrnnd` out of the same header, which is how libgmp does long division a limb at a time.
    // The dividend is two registers read as one number, and both of them are tied: the low half to
    // the output the quotient comes back in and the high half to the output the remainder comes back
    // in. So the instruction reads three registers and its text names one, and what says where the
    // other two are is a letter on each output and a number on each input.
    //
    // The two divisions this compiler already had are each two instructions, because a division in C
    // divides a number by a number of its own width and the machine divides a pair, so the high half
    // is filled first and that filling is what a template does not want. The one instruction on its
    // own is what a program that filled both halves itself is asking for.
    let source = "\
unsigned long f(unsigned long hi, unsigned long lo, unsigned long d, unsigned long *rem) {
  unsigned long q, r;
  asm (\"divq %4\" : \"=a\" (q), \"=d\" (r) : \"0\" (lo), \"1\" (hi), \"rm\" (d));
  *rem = r;
  return q;
}
";
    let body = body(&asm("udiv-qrnnd", source), "f");
    assert!(body.contains("divq"), "the division never reached the listing:\n{body}");
    assert!(!body.contains("idivq"), "the unsigned division became the signed one:\n{body}");
    assert!(
        !body.contains("cqto"),
        "the high half was filled over the top of the program's:\n{body}"
    );
    assert!(!body.contains("$0"), "a half of the dividend was zeroed rather than read:\n{body}");
}

#[test]
fn an_add_and_the_one_that_reads_its_carry_stay_next_to_each_other() {
    // `add_ssaaaa` and `sub_ddmmss` out of the same header, which is how libgmp adds and subtracts
    // numbers wider than a register. Both are two instructions in one template with a bit passed
    // between them, and the bit is not a register, so nothing in an operand vector says the second
    // waits for the first. What says it is the target's description of the condition state, which
    // the scheduler reads, and this test is here because getting that wrong is a wrong answer that
    // the listing on its own looks fine in.
    let source = "\
void f(unsigned long *sh, unsigned long *sl, unsigned long ah, unsigned long al,
       unsigned long bh, unsigned long bl) {
  unsigned long h, l;
  asm (\"addq %5,%q1\\n\\tadcq %3,%q0\"
       : \"=r\" (h), \"=&r\" (l)
       : \"0\" (ah), \"rme\" (bh), \"%1\" (al), \"rme\" (bl));
  *sh = h;
  *sl = l;
}

void g(unsigned long *sh, unsigned long *sl, unsigned long ah, unsigned long al,
       unsigned long bh, unsigned long bl) {
  unsigned long h, l;
  asm (\"subq %5,%q1\\n\\tsbbq %3,%q0\"
       : \"=r\" (h), \"=&r\" (l)
       : \"0\" (ah), \"rme\" (bh), \"1\" (al), \"rme\" (bl));
  *sh = h;
  *sl = l;
}
";
    let listing = asm("add-ssaaaa", source);
    for (name, first, second) in [("f", "addq", "adcq"), ("g", "subq", "sbbq")] {
        let body = body(&listing, name);
        let at = body.find(first).unwrap_or_else(|| panic!("no {first} in {name}:\n{body}"));
        let then = body.find(second).unwrap_or_else(|| panic!("no {second} in {name}:\n{body}"));
        assert!(at < then, "{second} came out in front of {first}:\n{body}");
        let between = &body[at..then];
        let over = between.matches('\n').count();
        assert_eq!(over, 1, "something got between the pair in {name}:\n{body}");
        assert!(!body.contains("$0"), "an operand was zeroed rather than read in {name}:\n{body}");
    }
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

#[test]
fn a_label_and_a_jump_back_to_it_are_written_as_a_loop() {
    // The loop libgmp carries a limb at a time, out of `MPN_INCR_U` in `gmp-impl.h`. A template that
    // jumps is not one instruction and cannot stay inside one block, so the label becomes a block of
    // its own and the jump ends the block it stands in. The pointer is written inside the loop and
    // read again at the top of it, which is why the operand it is in has to arrive at the label
    // rather than be assumed to still be where it started.
    let source = "void f(long *p) { long *d; \
                  asm volatile (\"\\n.Lasm_%=_top:\\n\\taddq $1, (%0)\\n\\tlea %c2(%0), %0\\n\\tjc .Lasm_%=_top\" \
                  : \"=r\" (d) : \"0\" (p), \"n\" (sizeof(long)) : \"memory\"); }\n";
    let body = body(&asm("template-loop", source), "f");
    assert!(body.contains("addq\t$1, ("), "the add into memory never reached the listing:\n{body}");
    assert!(body.contains("leaq\t8("), "the step along a limb never reached the listing:\n{body}");
    let Some(at) = body.find("\tjb\t") else { panic!("the carry never became a jump:\n{body}") };
    let to = body[at + 4..].lines().next().expect("a jump names where it goes").trim();
    // The arm the carry takes is a critical edge, so the pass that splits those stands a block of its
    // own on it and the jump arrives there rather than at the top of the loop. That block holds the
    // moves the arm turns into, of which there are none here, and the jump on to where the template
    // said, so the loop is one hop further round than the template wrote it.
    let to = landing(&body, to).unwrap_or(to);
    let back = body[..at].contains(&format!("\n{to}:"));
    assert!(back, "the jump goes to {to}, which is not a place above it:\n{body}");
}

/// Where a block that holds nothing but a jump sends whatever arrived at it.
fn landing<'a>(body: &'a str, label: &str) -> Option<&'a str> {
    let at = body.find(&format!("\n{label}:\n"))?;
    let rest = body[at + label.len() + 3..].trim_start_matches(['\t', ' ']);
    rest.strip_prefix("jmp")?.lines().next().map(str::trim)
}

#[test]
fn a_jump_with_no_condition_on_it_says_what_is_missing() {
    // Nothing reaches whatever the jump goes past, and a template that writes one is asking for a
    // shape this does not build yet. Saying so is the point: the alternative is a listing that keeps
    // the instructions and drops the jump, which is a program that builds and does not run.
    let source = "void f(void) { asm volatile (\"jmp .Lgone\\n.Lgone:\"); }\n";
    let (ok, _, said) = run("template-jump", source);
    assert!(!ok, "an unconditional jump inside a template was accepted");
    assert!(said.contains("which nothing here assembles"), "{said}");
}
