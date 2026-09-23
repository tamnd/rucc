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

/// Whether the listing puts a zero in a register, which is what an output nothing wrote is owed.
/// The back end spells that either as a move of a zero or, where the condition state is free, as an
/// exclusive or of a register with itself, and which of the two is in front of a template is not
/// what a test of the template is about.
fn zeroed(body: &str) -> bool {
    body.contains("$0,") || body.contains("xor")
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
fn an_instruction_on_an_operand_in_memory_works_on_the_object_where_it_lives() {
    // tcc's `tests/tcctest.c`, which counts a static local up from assembly to show that one is
    // reachable from there at all. The object is in memory and stays there, so what the listing
    // has is one instruction adding one to an address and not a load, an addition and a store.
    let source = "int f(void) { static int n = 41; asm (\"incl %0\" : \"+m\" (n)); return n; }\n";
    let text = asm("counted", source);
    let body = body(&text, "f");
    assert!(body.contains("incl\tn.0(%rip)"), "{body}");
}

#[test]
fn a_bit_set_in_memory_is_the_one_instruction_that_sets_it() {
    // How a C library wrote `sigaddset` before there was a builtin for it, and how tcc's test suite
    // still does: the bit number in a register and the set it is counted in, in memory.
    let source = "void f(unsigned *set, int bit) { \
                  asm (\"btsl %1,%0\" : \"+m\" (*set) : \"Ir\" (bit) : \"cc\", \"flags\"); }\n";
    let text = asm("bits", source);
    let body = body(&text, "f");
    assert!(body.contains("btsl\t%"), "{body}");
}

#[test]
fn an_operand_in_memory_the_template_names_as_a_register_is_refused() {
    // `%h0` is the byte above the low byte of a register, and an object in memory is not in one.
    let source = "void f(short *p) { asm (\"xchgb %b0,%h0\" : \"+m\" (*p)); }\n";
    let (ok, _, said) = run("half", source);
    assert!(!ok, "the high byte of an object in memory was accepted");
    assert!(said.contains("instructions in its template"), "{said}");
}

#[test]
fn an_address_handed_in_with_p_is_the_register_it_is_in() {
    // tcc's `tests/tcctest.c`, word for word apart from the type. `%P1` is the operand without the
    // punctuation gcc would put round a constant, and a register has none, so it is `%1`.
    let source = "long f(void) { long ret; int var; \
                  asm volatile (\"mov %P1,%0\" : \"=r\" (ret) : \"p\" (&var)); \
                  return ret == (long) &var; }\n";
    let text = asm("address", source);
    let body = body(&text, "f");
    assert!(body.contains("leaq\t"), "the address was never taken:\n{body}");
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
fn an_operand_read_at_less_than_its_width_is_the_bottom_of_it() {
    // `memcpy1` in tcc's `tcctest.c`, which copies the odd bytes at the end of a buffer by testing
    // the low bits of the count. The count is a `size_t` and the test reads one byte of it, and
    // every bit of that byte is one the count put there.
    let source = "int f(unsigned long n) { int r = 0; \
                  asm (\"testb $2,%b1\\n\\tje 1f\\n\\tmovl $1,%0\\n1:\" \
                  : \"+r\" (r) : \"q\" (n)); return r; }\n";
    let body = body(&asm("read-narrow", source), "f");
    assert!(body.contains("testb\t$2,"), "the byte test never reached the listing:\n{body}");
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
    assert!(!zeroed(&body), "a multiplicand was zeroed rather than read:\n{body}");
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
    assert!(!zeroed(&body), "a half of the dividend was zeroed rather than read:\n{body}");
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
        assert!(!zeroed(&body), "an operand was zeroed rather than read in {name}:\n{body}");
    }
}

#[test]
fn an_output_read_before_it_is_written_holds_a_zero_rather_than_nothing() {
    // The same addition as above on an output written `=`, which says the assembly writes the
    // operand and never reads it, while the instruction reads it before it writes it. What is in it
    // there is undefined and the program said so by writing `=` rather than `+`, so this was refused
    // for a while on the grounds that a read of something nothing filled is a mistake. It is not
    // this compiler's to call. GCC 16.2.0 accepts the same statement without a word and adds
    // whatever the register it picked was holding, and a program that means it is real: libgmp's
    // `add_mssaaaa` writes `sbb %0, %0` to get the borrow bit, where what the register held cannot
    // change the answer. So it is accepted and the operand is given the zero an output nothing wrote
    // gets, because undefined is not the same as absent and the allocator is owed a definition in
    // front of every use.
    let source = "long f(long x) { asm (\"addq %1, %0\" : \"=r\" (x) : \"r\" (x)); return x; }\n";
    let body = body(&asm("write-only", source), "f");
    assert!(body.contains("addq"), "the template never reached the listing:\n{body}");
    assert!(zeroed(&body), "the operand it reads was never given a value:\n{body}");
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

/// What a program writes when its assembler was older than the instruction it wants. libwebp writes
/// exactly this in `src/dsp/cpu.c` and in `sharpyuv/sharpyuv_cpu.c`: the three bytes are `xgetbv`,
/// which is how a program asks whether the operating system has agreed to save the wide registers,
/// and the mnemonic arrived after the code that asks did. The bytes are already the answer, so what
/// is owed here is to write them back out rather than to assemble anything.
#[test]
fn a_byte_directive_in_a_template_reaches_the_listing_as_the_bytes_it_names() {
    let source = "\
unsigned long long f(unsigned int n) {
  unsigned int lo, hi;
  __asm__ volatile (\".byte 0x0f, 0x01, 0xd0\" : \"=a\"(lo), \"=d\"(hi) : \"c\"(n));
  return ((unsigned long long)hi << 32) | lo;
}
";
    let text = asm("byte", source);
    let body = body(&text, "f");
    assert!(
        body.contains(".byte\t0x0f, 0x01, 0xd0"),
        "the bytes never reached the listing:\n{body}"
    );
    // And the registers, which are the other half of it. The bytes say nothing about where the
    // operands go, so the constraint letters say it: the argument has to arrive in `rcx` in front
    // of the bytes and the answer has to be read out of `rax` and `rdx` behind them.
    let front = body.split(".byte").next().expect("something in front of the bytes");
    assert!(
        front.contains("%rcx") || front.contains("%ecx"),
        "the input never reached `rcx`:\n{body}"
    );
}

/// A number that is not a byte is refused rather than truncated, for the reason a fill byte on an
/// alignment is: a program that wrote one meant something this does not do, and writing the low
/// eight bits of it would be a program that builds and is not the one that was written.
#[test]
fn a_byte_directive_that_is_not_bytes_says_so_rather_than_truncating() {
    let source = "void f(void) { __asm__ volatile (\".byte 0x0f01\"); }\n";
    let (ok, _, said) = run("byte-wide", source);
    assert!(!ok, "a number wider than a byte was accepted");
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

#[test]
fn local_labels_and_a_jump_on_the_sign_are_written_as_a_loop() {
    // The count down in tcc's `strncat1`, cut down to the loop. The label shares its line with the
    // instruction behind it, the jumps name the nearest `1` behind and the nearest `2` in front, and
    // the way out is on the sign, which no C condition asks about on its own.
    let source = "int f(int n) { \
                  asm (\"1:\\tdec %0\\n\\tjs 2f\\n\\tjne 1b\\n2:\" : \"+r\" (n)); return n; }\n";
    let body = body(&asm("local-labels", source), "f");
    assert!(body.contains("decl\t"), "the count down never reached the listing:\n{body}");
    assert!(body.contains("\tjs\t"), "the jump on the sign never reached the listing:\n{body}");
    let jumps = body.matches("\tjne\t").count() + body.matches("\tje\t").count();
    assert!(jumps > 0, "the jump back never reached the listing:\n{body}");
}

#[test]
fn a_string_instruction_is_written_with_its_registers_where_it_wants_them() {
    // tcc's `strcpy` and the copy in its `memcpy1`. The registers are the ones the instructions
    // name for themselves, so what the listing has to show is the pointers arriving in `rsi` and
    // `rdi`, the count in `rcx`, and the instructions written the way the template wrote them.
    let source = "char *copy(char *dest, const char *src) { int d0, d1, d2; \
                  asm volatile (\"1:\\tlodsb\\n\\tstosb\\n\\ttestb %%al,%%al\\n\\tjne 1b\" \
                  : \"=&S\" (d0), \"=&D\" (d1), \"=&a\" (d2) : \"0\" (src), \"1\" (dest) : \"memory\"); \
                  return dest; }\n\
                  void move(void *to, const void *from, unsigned long n) { long d0, d1, d2; \
                  asm volatile (\"rep ; movsl\" : \"=&c\" (d0), \"=&D\" (d1), \"=&S\" (d2) \
                  : \"0\" (n / 4), \"1\" (to), \"2\" (from) : \"memory\"); }\n";
    let text = asm("string", source);
    let copy = body(&text, "copy");
    assert!(copy.contains("lodsb") && copy.contains("stosb"), "the copy is missing:\n{copy}");
    let moved = body(&text, "move");
    assert!(moved.contains("rep movsl"), "the repeated move is missing:\n{moved}");
    assert!(moved.contains("%rcx") || moved.contains("%ecx"), "the count is nowhere:\n{moved}");
}

#[test]
fn an_operand_written_twice_is_read_where_the_second_write_left_it() {
    // The tail of tcc's `memcpy2`, where both pointers are stepped by one instruction and then by
    // the next. Each instruction writes the two operands again, so what the second one reads has
    // to be what the first one left and not what the statement handed in.
    let source = "void tail(char *to, const char *from) { long d1, d2; \
                  asm volatile (\"movsw\\n\\tmovsb\" : \"=&D\" (d1), \"=&S\" (d2) \
                  : \"0\" (to), \"1\" (from) : \"memory\"); }\n";
    let body = body(&asm("twice", source), "tail");
    let (Some(word), Some(byte)) = (body.find("movsw"), body.find("movsb")) else {
        panic!("the two moves never reached the listing:\n{body}");
    };
    // Nothing is put back into `rsi` or `rdi` between the two, since both already hold what the
    // second one reads.
    let between = &body[word..byte];
    assert!(!between.contains("%rsi") && !between.contains("%rdi"), "{body}");
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

#[test]
fn an_add_with_carry_against_a_constant_is_the_instruction_it_names() {
    // The third word of a number three words wide, which is `add_sssaaaa` in libgmp's `longlong.h`.
    // There is nothing to add there except the bit that fell off the word below, and a constant zero
    // is how the instruction that adds only the bit is spelled.
    let source = "unsigned long f(unsigned long s, unsigned long a, unsigned long b) { \
                  unsigned long t; \
                  asm (\"add %4, %1\\n\\tadc $0, %q0\" \
                  : \"=r\" (s), \"=&r\" (t) : \"0\" (s), \"1\" (a), \"r\" (b)); return s; }\n";
    let body = body(&asm("adc-constant", source), "f");
    assert!(body.contains("adcq\t$0,"), "the add with carry never reached the listing:\n{body}");
}

#[test]
fn an_output_a_template_also_reads_is_read_as_a_zero() {
    // `sbb %0, %0` in libgmp's `add_mssaaaa`, which subtracts a register from itself and is asking
    // for the borrow bit rather than for the number, so what the register held does not matter. It
    // is an output and nothing is tied to it, so the statement never said what is in it, and what
    // the allocator is owed is still a definition in front of the use.
    let source = "long f(long x, long y) { long m; \
                  asm (\"add %2, %1\\n\\tsbb %q0, %q0\" : \"=r\" (m), \"+r\" (x) : \"r\" (y)); \
                  return m; }\n";
    let body = body(&asm("output-read", source), "f");
    assert!(body.contains("sbbq"), "the subtract with borrow never reached the listing:\n{body}");
    assert!(zeroed(&body), "the operand it reads was never given a value:\n{body}");
}

#[test]
fn the_two_halves_of_a_word_are_exchanged_in_the_register_that_has_both() {
    // `ByteSwap16` in femtolisp's `llt/utils.h`, which is how a C library written before
    // `__builtin_bswap16` turned a sixteen bit number round. It is the only template in the corpus
    // that names half a register rather than an amount of one, and the half it names is the byte
    // above the low one, which only the first four registers of this machine have. So the operand
    // is in `rax` because the description of the instruction says so, and the value the statement
    // handed it is carried there first.
    let source = "unsigned short f(unsigned short x) { \
                  __asm(\"xchgb %b0,%h0\" : \"=Q\" (x) : \"0\" (x)); return x; }\n";
    let body = body(&asm("byte-swap", source), "f");
    assert!(body.contains("xchgb\t%al, %ah"), "the exchange never reached the listing:\n{body}");
}

#[test]
fn the_bytes_of_a_long_are_turned_round_in_the_register_that_holds_it() {
    // `swab32` in tcc's `tcctest.c`, which swaps the low two bytes, rotates the halves past each
    // other and swaps the low two again. The swaps write sixteen bits of a thirty two bit object,
    // which is right because the object arrived in the register and the top half is left alone.
    let source = "unsigned f(unsigned x) { \
                  __asm__(\"xchgb %b0,%h0\\n\\trorl $16,%0\\n\\txchgb %b0,%h0\" \
                  : \"=q\" (x) : \"0\" (x)); return x; }\n";
    let body = body(&asm("byte-swap-long", source), "f");
    assert!(body.contains("xchgb\t%al, %ah"), "the exchange never reached the listing:\n{body}");
    assert!(body.contains("rorl\t$16, %eax"), "the rotate never reached the listing:\n{body}");
}

#[test]
fn a_high_byte_of_one_word_and_a_low_byte_of_another_is_refused() {
    // There is no instruction here that exchanges a byte of one register with a byte of another, so
    // a template asking for one is told rather than being read as the exchange within a register
    // that it resembles. Getting this wrong would be a program that builds and swaps the wrong two
    // bytes, which is the one outcome worth refusing a template to avoid.
    let source = "void f(unsigned short a, unsigned short b) { \
                  __asm(\"xchgb %b0,%h1\" : \"+Q\" (a), \"+Q\" (b)); }\n";
    let (ok, _, said) = run("byte-swap-across", source);
    assert!(!ok, "a byte of one register exchanged with a byte of another was accepted");
    assert!(said.contains("which nothing here assembles"), "{said}");
}
