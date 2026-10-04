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
    run_with(what, source, &[])
}

/// The same, with more said on the command line.
fn run_with(what: &str, source: &str, flags: &[&str]) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(flags)
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
fn an_input_tied_to_an_output_by_name_shares_its_register() {
    // `"[sum]"` as an input constraint is the same tie as `"0"`, written without counting. The
    // kernel's `csum_ipv6_magic` ties its running sum this way, and the name used to reach the
    // backend as a constraint it could not place. An unknown name is gcc's error.
    let text = asm(
        "tied-by-name",
        concat!(
            "unsigned long f(const unsigned long *a, unsigned long rest) {\n",
            "  unsigned long sum;\n",
            "  asm (\"addq (%[a]),%[sum]\" : [sum] \"=r\" (sum) : \"[sum]\" (rest), [a] \"r\" (a));\n",
            "  return sum;\n",
            "}\n",
        ),
    );
    let body = body(&text, "f");
    assert!(body.contains("addq\t(%r"), "{body}");
    let (ok, _, said) = run(
        "tied-to-nothing",
        "int f(int x) { asm (\"\" : [y] \"=r\" (x) : \"[z]\" (x)); return x; }\n",
    );
    assert!(!ok && said.contains("undefined named operand 'z'"), "{said}");
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
fn two_locals_in_memory_are_each_written_where_they_are() {
    // The kernel's `has_fpu` in `arch/x86/boot/cpuflags.c`. Only one operand can be the
    // instruction's own and be named from the stack pointer, so the other has its address put in
    // a register first, and each store goes to the object it names.
    let source = "int f(void) { unsigned short fcw = -1, fsw = -1;\n\
                  asm volatile (\"fninit ; fnstsw %0 ; fnstcw %1\" : \"+m\" (fsw), \"+m\" (fcw));\n\
                  return fsw == 0 && (fcw & 0x103f) == 0x003f; }\n";
    for level in ["-O0", "-O2"] {
        let (ok, text, said) = run_with("two-locals", source, &[level]);
        assert!(ok, "the compiler refused the fixture at {level}:\n{said}");
        let body = body(&text, "f");
        let lea = body.lines().find(|line| line.contains("leaq")).expect("an address is taken");
        let reg = lea.rsplit(", ").next().expect("the lea names a register").trim();
        assert!(body.contains("fnstsw -") && body.contains("(%rsp) ;"), "{level}:\n{body}");
        assert!(body.contains(&format!("fnstcw ({reg})")), "{level}:\n{body}");
    }
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
fn a_p_on_an_operand_in_memory_is_the_operand_as_it_is() {
    // The kernel's alternatives before 6.8 write `prefetchw %P0` and `.byte 0x66; clflush %P0`
    // against an `"m"` operand. gcc prints the operand as it would bare, less the `(%rip)` of a
    // constant address, and the modifier used to be refused here.
    let text = asm(
        "p-on-memory",
        concat!(
            "char g;\n",
            "void a(const char *x) { asm volatile (\"prefetchw %P0\" : : \"m\" (*x)); }\n",
            "void b(void) { asm volatile (\"prefetchw %P0\" : : \"m\" (g)); }\n",
        ),
    );
    assert!(body(&text, "a").contains("prefetchw (%r"), "{text}");
    assert!(body(&text, "b").contains("prefetchw g(%rip)"), "{text}");
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
fn the_address_of_a_global_handed_in_with_p_is_the_name_itself() {
    // 6.1's `this_cpu_read_stable` writes `movq %%gs:%P[var], %[val]` with `"p" (&current_task)`.
    // gcc takes an address the linker knows as a constant, so `%P` prints the bare name, and the
    // template used to be refused here because the address went in a register.
    let source = "extern long current_task;\n\
                  long f(void) { long v; \
                  asm (\"movq %%gs:%P[var], %[val]\" : [val] \"=r\" (v) : [var] \"p\" (&current_task)); \
                  return v; }\n";
    let text = asm("percpu-stable", source);
    let body = body(&text, "f");
    assert!(body.contains("movq %gs:current_task, %"), "{body}");
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
    // And the narrow half of the same question, which is an instruction that writes a quarter of
    // the object it was given and leaves the rest holding whatever was there. gcc writes that one
    // and the program it writes it for is relying on what was in the register before rather than
    // on anything it said, so this refuses it until something real asks for it. Half of a `long`
    // is not the same question, because a write of four bytes clears the four above them.
    let part = "long f(long x) { long n; asm (\"bsf %w1, %w0\" : \"=r\" (n) : \"r\" (x)); \
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
fn an_output_read_before_it_is_written_holds_the_input_it_shares_with() {
    // The same addition as above on an output written `=`, which says the assembly writes the
    // operand and never reads it, while the instruction reads it before it writes it. What is in it
    // there is undefined and the program said so by writing `=` rather than `+`, so this was refused
    // for a while on the grounds that a read of something nothing filled is a mistake. It is not
    // this compiler's to call. GCC 16.2.0 accepts the same statement without a word and adds
    // whatever the register it picked was holding, and a program that means it is real: libgmp's
    // `add_mssaaaa` writes `sbb %0, %0` to get the borrow bit, where what the register held cannot
    // change the answer. What it picks here is the register of the one input, since nothing stops
    // the two sharing it, so the sum is twice the argument and that is what is read.
    let source = "long f(long x) { asm (\"addq %1, %0\" : \"=r\" (x) : \"r\" (x)); return x; }\n";
    let body = body(&asm("write-only", source), "f");
    assert!(body.contains("addq"), "the template never reached the listing:\n{body}");
    assert!(!zeroed(&body), "the operand it reads is not the input it shares with:\n{body}");
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
fn an_alignment_with_a_limit_is_kept_as_the_text_it_was() {
    // A third argument is the most padding worth writing, which the instruction this reads an
    // alignment into has no room for. So the template is kept as text and the assembler that
    // reads the listing does all of what it says, the way gcc's does.
    let source = "void f(void) { asm volatile (\".p2align 4, 0x90, 8\"); }\n";
    let text = asm("align-fill", source);
    assert!(body(&text, "f").contains("\t.p2align 4, 0x90, 8\n"), "{text}");
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

/// A number that is not a byte is not read into an instruction, and the template is kept as the
/// text it was. What becomes of the number is then the assembler's answer, which is the same
/// answer gas gives: the low eight bits.
#[test]
fn a_byte_directive_that_is_not_bytes_is_kept_as_the_text_it_was() {
    let source = "void f(void) { __asm__ volatile (\".byte 0x0f01\"); }\n";
    let text = asm("byte-wide", source);
    assert!(body(&text, "f").contains("\t.byte 0x0f01\n"), "{text}");
}

#[test]
fn a_long_double_operand_is_pushed_onto_the_x87_stack_and_popped_off_it() {
    // The statement glibc's old `<bits/mathinline.h>` wrote for `sqrtl`. The input is tied to the
    // output, so the template pops it and pushes the answer, and the answer is popped into its slot.
    let source = "long double f(long double x) { long double r; \
                  __asm__ (\"fsqrt\" : \"=t\" (r) : \"0\" (x)); return r; }\n";
    let body = body(&asm("x87", source), "f");
    let push = body.find("fldt").unwrap_or_else(|| panic!("nothing was pushed:\n{body}"));
    let run = body.find("fsqrt").unwrap_or_else(|| panic!("the template is missing:\n{body}"));
    let pop = body[run..].find("fstpt").unwrap_or_else(|| panic!("nothing was popped:\n{body}"));
    assert!(push < run && pop > 0, "{body}");
}

#[test]
fn an_x87_input_the_template_leaves_on_the_stack_is_refused() {
    // Nothing ties the input to an output and the clobber list does not say it is popped, so the
    // template leaves it behind, and nothing here pops what a template left.
    let source = "void f(long double x) { __asm__ volatile (\"fld %%st\" : : \"t\" (x)); }\n";
    let (ok, _, said) = run("x87-left", source);
    assert!(!ok, "a statement that leaves the stack deeper than it found it was accepted");
    assert!(said.contains("operand"), "{said}");
}

#[test]
fn an_asm_goto_that_jumps_names_the_block_its_label_became() {
    // `%l0` is written as the local label of the block the label is, which is the only name the
    // block has once the layout has numbered it.
    let source =
        "int f(int x) { asm goto (\"jmp %l0\" : : : : away); return x; away: return 0; }\n";
    let body = body(&asm("goto", source), "f");
    let target = body
        .lines()
        .find_map(|line| line.trim().strip_prefix("jmp "))
        .unwrap_or_else(|| panic!("the template is missing:\n{body}"));
    assert!(body.contains(&format!("\n{target}:")), "{target} is not a block:\n{body}");
}

#[test]
fn an_asm_goto_with_nothing_in_it_falls_through() {
    // What the torture suite writes to tell the optimizer a label can be reached without saying
    // how. There is no instruction in the template to jump with, so the function returns `x`.
    let source = "int f(int x) { asm goto (\"\" : : : : away); return x; away: return 0; }\n";
    let body = body(&asm("goto-empty", source), "f");
    assert!(body.contains("ret"), "{body}");
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
fn a_jump_with_no_condition_on_it_is_kept_as_the_text_it_was() {
    // Nothing reaches whatever the jump goes past, which is a shape the blocks this reads a template
    // into do not have. The text has it, so the text is kept, jump and label both.
    let source = "void f(void) { asm volatile (\"jmp .Lgone\\n.Lgone:\"); }\n";
    let text = asm("template-jump", source);
    let f = body(&text, "f");
    assert!(f.contains("\tjmp .Lgone\n\t.Lgone:\n"), "{text}");
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
    // is an output written `&`, so it shares a register with no input and the statement never said
    // what is in it, and what the allocator is owed is still a definition in front of the use.
    let source = "long f(long x, long y) { long m; \
                  asm (\"add %2, %1\\n\\tsbb %q0, %q0\" : \"=&r\" (m), \"+r\" (x) : \"r\" (y)); \
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
fn a_truth_value_is_the_byte_it_is_kept_in() {
    // tcc's test that the width of a `_Bool` output is a byte: `sete` writes one and that byte is
    // the answer.
    let source = "_Bool f(void) { _Bool b; \
                  asm volatile (\"cmp %1,%2; sete %0\" : \"=a\" (b) : \"r\" (1), \"r\" (2)); \
                  return b; }\n";
    let body = body(&asm("truth-byte", source), "f");
    assert!(body.contains("sete\t%al"), "the byte never reached the listing:\n{body}");
}

#[test]
fn a_constant_an_assembler_works_out_is_the_number_it_comes_to() {
    // tcc checks that its assembler reads `0x1E-1` as three tokens rather than one number.
    let source = "void f(void) { asm volatile (\"mov $0x1E-1,%eax\"); }\n";
    let body = body(&asm("sum-immediate", source), "f");
    assert!(body.contains("$29, %eax"), "the constant was not worked out:\n{body}");
}

#[test]
fn directives_about_names_in_a_function_are_directives_of_the_file() {
    // tcc's `asm_test` equates a weak name to a function from inside another function, and the
    // name it equates to is the global one whatever the function has in scope.
    let source = "void base_func(void) {} \
                  void f(void) { int base_func = 42; (void)base_func; \
                  asm volatile (\".weak alias3\\n.set alias3, base_func\"); }\n";
    let text = asm("names-in-function", source);
    assert!(text.contains(".weak\talias3"), "the weak name is missing:\n{text}");
    assert!(text.contains("alias3,base_func"), "the equate is missing:\n{text}");
}

#[test]
fn an_equate_at_file_scope_defines_a_name_the_file_only_declared() {
    // tcc's `asm_test` calls `override_func1`, which C declares `extern` and a file-scope `asm`
    // defines with `.set`. The declaration was in the module first, and the equate was dropped.
    let source = "void base_func(void) {} extern void over(void); \
                  asm(\".weak over\\n.set over, base_func\"); \
                  void f(void) { over(); }\n";
    let text = asm("equate-declared", source);
    assert!(text.contains(".weak\tover"), "the weak name is missing:\n{text}");
    assert!(text.contains("over,base_func"), "the equate is missing:\n{text}");
}

#[test]
fn an_operand_in_memory_is_written_where_it_is_and_four_bytes_past_it() {
    // `mconstraint_test` in tcc's `tcctest.c`. The template takes the address of an `"m"` operand,
    // loads the word four bytes in through it into a `long` with `movl 4(%0),%k0`, and then
    // stores a constant at the operand and at `4%2`, which is four bytes past it. A write of four
    // bytes clears the top of the register, so the `long` holds the word and nothing else.
    let source = "struct two { int a; int b; }; \
                  unsigned long f(struct two *p) { unsigned long ret; unsigned a[2]; a[0] = 0; \
                  __asm__ volatile (\"lea %2,%0; movl 4(%0),%k0; addl %2,%k0; \
                  movl $51,%2; movl $52,4%2; movl $63,%1\" \
                  : \"=&r\" (ret), \"=m\" (a) : \"m\" (*p)); return ret + a[0]; }\n";
    let body = body(&asm("memory-past", source), "f");
    assert!(body.contains("movl\t$51, ("), "the store at the operand is missing:\n{body}");
    assert!(body.contains("movl\t$52, 4("), "the store past the operand is missing:\n{body}");
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

#[test]
fn a_call_from_a_template_is_a_call_and_reads_the_input_sharing_its_output() {
    // tcc's test of a name only a template uses. It passes its output `%0` to `getenv`, which is
    // the string only because gcc gives the output and the one input the same register.
    let source = "char *f(void) { static char str[] = \"PATH\"; char *s; \
                  asm volatile (\"push %%rdi; push %%rdi; mov %0, %%rdi;call getenv@plt;pop %%rdi; pop %%rdi\" \
                  : \"=a\" (s) : \"r\" (str)); return s; }\n";
    let body = body(&asm("call-out", source), "f");
    assert!(body.contains("call\tgetenv"), "the call never reached the listing:\n{body}");
    assert!(!zeroed(&body), "the output was read as nothing rather than as the input:\n{body}");
}

#[test]
fn an_input_pinned_to_a_register_is_there_for_the_call_the_template_makes() {
    // The kernel's way of passing an argument to a call it makes from a template, `"D" (x)` beside
    // `call g`. Nothing in the template reads `rdi`, so without the call reading it the move of the
    // second argument into it was a move nobody read and was left out.
    let source =
        "void g(void); void f(long a, long x) { asm volatile (\"call g\" : : \"D\" (x)); }\n";
    let (ok, text, said) = run_with("pinned-call", source, &["-O2"]);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    let body = body(&text, "f");
    let moved = body.find("movq\t%rsi, %rdi").expect("the argument never reached rdi");
    let called = body.find("call\tg").expect("the call never reached the listing");
    assert!(moved < called, "the argument was put in rdi after the call:\n{body}");
}

#[test]
fn a_clobber_list_does_not_move_what_a_line_reads() {
    // The listing names a register by where it is among the instruction's operands, and the
    // clobbers used to go in among them, so every read behind them was one place off. This was
    // written as `movq %rcx, %rax`, a move out of the register the list said was destroyed.
    let source = "long f(long x) { long y; asm (\"movq %1, %0\" : \"=r\" (y) : \"r\" (x) : \"rcx\"); \
                  return y + x; }\n";
    let (ok, text, said) = run_with("clobber-read", source, &["-O2"]);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    let body = body(&text, "f");
    assert!(body.contains("movq\t%rdi, %"), "the move does not read x:\n{body}");
    assert!(!body.contains("movq\t%rcx"), "the move reads the clobbered register:\n{body}");
}

#[test]
fn a_register_the_template_writes_and_also_clobbers_is_written_once() {
    // `rcx` written by name and named in the clobber list as a program has to name it. The second
    // line read it back as the clobber rather than as what the first line put there, and the
    // template was gone above -O0.
    let source = "long f(long x) { long y; asm volatile (\"movq %1, %%rcx\\n\\tmovq %%rcx, %0\" \
                  : \"=r\" (y) : \"r\" (x) : \"rcx\"); return y; }\n";
    for level in ["-O0", "-O2"] {
        let (ok, text, said) = run_with("clobbered-write", source, &[level]);
        assert!(ok, "the compiler refused the fixture:\n{said}");
        let body = body(&text, "f");
        assert!(body.contains("movq\t%rdi, %rcx"), "x never reached rcx at {level}:\n{body}");
        assert!(body.contains("movq\t%rcx, %"), "rcx was never read at {level}:\n{body}");
    }
}

#[test]
fn the_register_modifiers_the_kernel_uses_are_spelled_the_way_gcc_spells_them() {
    // `%V` is the retpoline thunk's name, `%a` a register as an address and `%z` the suffix for
    // the operand's width. Each of them used to send the whole function back as unsupported.
    let source = "void (*fp)(void);\n\
                  void thunk(void) { asm volatile (\"call __x86_indirect_thunk_%V0\" : : \"r\" (fp)); }\n\
                  void load(long *p) { asm volatile (\"incq %a0\" : : \"r\" (p) : \"memory\"); }\n\
                  int wide(int x) { asm (\"add%z0 $1, %0\" : \"+r\" (x)); return x; }\n\
                  short half(short x) { asm (\"add%z0 $1, %0\" : \"+r\" (x)); return x; }\n";
    let (ok, text, said) = run_with("register-modifiers", source, &["-O2"]);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    let thunk = body(&text, "thunk");
    assert!(thunk.contains("call __x86_indirect_thunk_r"), "no bare register:\n{thunk}");
    assert!(!thunk.contains("thunk_%"), "the register kept its percent:\n{thunk}");
    assert!(body(&text, "load").contains("incq (%rdi)"), "not an address:\n{text}");
    assert!(body(&text, "wide").contains("addl $1, %e"), "not a long:\n{text}");
    assert!(body(&text, "half").contains("addw $1, %"), "not a word:\n{text}");
}

/// A `+r` on the destination of a `bsr` is what the kernel's `fls64` leans on: the search of zero
/// leaves the destination alone, so the -1 that went in comes out and the answer is zero. The
/// source is computed in the function so that it is in a register the destination could take,
/// which is how the -1 got lost: `get_order` of anything under a page came back as one.
#[test]
fn a_search_tied_to_its_starting_value_keeps_it_for_zero() {
    let source = "int order(unsigned int size) { unsigned long x = size; x--; x >>= 12; \
                  int bit = -1; asm (\"bsrq %1,%q0\" : \"+r\" (bit) : \"rm\" (x)); return bit + 1; }\n";
    for level in ["-O1", "-O2", "-Os"] {
        let (ok, text, said) = run_with("tied-search", source, &[level]);
        assert!(ok, "the compiler refused the fixture:\n{said}");
        let body = body(&text, "order");
        let search = body.lines().find(|line| line.contains("bsrq")).expect("the search is there");
        let dest = search.rsplit(',').next().expect("two operands").trim().to_string();
        let src = search.split(',').next().expect("two operands").split_whitespace().last();
        assert_ne!(src, Some(dest.as_str()), "the source took the destination at {level}:\n{body}");
        // The -1 is in the destination when the search runs.
        let wide = dest.replace("%r", "%e").replace("%ee", "%e");
        let set = format!("movl\t$-1, {wide}");
        assert!(
            body.contains(&set) || body.contains(&format!("movq\t$-1, {dest}")),
            "{level}:\n{body}"
        );
    }
}

/// A global in memory is named from the instruction pointer, the way gcc names it, when the file
/// can reach it that way. The kernel's paravirt calls are `call *%[paravirt_opptr]` against
/// `"m" (pv_ops.op)`, and what patches them into direct calls reads only `ff 15`, which is
/// `call *x(%rip)`. Under `-fPIC` the name is read through the GOT and stays in a register.
#[test]
fn a_global_in_memory_is_named_from_the_instruction_pointer() {
    let source = "struct ops { void (*a)(void); unsigned long (*b)(void); } pv;\n\
                  unsigned long f(void) { unsigned long r; \
                  asm volatile (\"call *%[p]\" : \"=a\" (r) : [p] \"m\" (pv.b) : \"memory\"); return r; }\n";
    for flags in [&["-O2", "-fno-PIE"][..], &["-O2", "-mcmodel=kernel", "-fno-PIE"]] {
        let (ok, text, said) = run_with("near-memory", source, flags);
        assert!(ok, "the compiler refused the fixture:\n{said}");
        assert!(body(&text, "f").contains("call *pv+8(%rip)"), "{flags:?}:\n{text}");
    }
    let (ok, text, said) = run_with("far-memory", source, &["-O2", "-fPIC"]);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    assert!(body(&text, "f").contains("call *(%r"), "{text}");
}

#[test]
fn an_address_the_template_spells_as_a_name_is_not_built_as_well() {
    let source = "struct ops { void (*a)(void); unsigned long (*b)(void); } pv;\n\
                  unsigned long f(void) { unsigned long r; \
                  asm volatile (\"call *%[p]\" : \"=a\" (r) : [p] \"m\" (pv.b) : \"memory\"); return r; }\n\
                  long arr[4];\n\
                  int has(void) { asm goto (\"testb $1, %a0\\n\\tjnz %l1\" : : \"i\" (&arr[1]) : : yes); \
                  return 0; yes: return 1; }\n";
    let flags = ["-O2", "-mcmodel=kernel", "-fno-PIE"];
    let (ok, text, said) = run_with("unread-address", source, &flags);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    assert!(!body(&text, "f").contains("$pv"), "the address was built anyway:\n{text}");
    assert!(!body(&text, "has").contains("$arr"), "the address was built anyway:\n{text}");
}

#[test]
fn a_constant_can_be_negated_and_a_memory_operand_read_eight_bytes_on() {
    let source = "long arr[2];\n\
                  long less(long x) { asm (\"addq $%n1, %0\" : \"+r\" (x) : \"i\" (4)); return x; }\n\
                  long high(long *p) { long r; asm (\"movq %H1, %0\" : \"=r\" (r) : \"m\" (*p)); \
                  return r; }\n\
                  void *at(void) { void *r; asm (\"leaq %a1, %0\" : \"=r\" (r) : \"i\" (arr)); \
                  return r; }\n\
                  int has(void) { asm goto (\"testb $1, %a0\\n\\tjnz %l1\" : : \"i\" (&arr[1]) : : yes); \
                  return 0; yes: return 1; }\n";
    let (ok, text, said) = run_with("constant-modifiers", source, &["-O2"]);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    assert!(body(&text, "less").contains("addq $-4, %"), "not negated:\n{text}");
    assert!(body(&text, "high").contains("movq 8(%rdi), %"), "not eight bytes on:\n{text}");
    // A name as an address is reached from the instruction pointer, which gcc adds itself, and
    // the kernel's `static_cpu_has` leans on that in code that runs before it is relocated.
    assert!(body(&text, "at").contains("leaq arr(%rip), %"), "not rip-relative:\n{text}");
    assert!(body(&text, "has").contains("testb $1, arr+8(%rip)"), "not rip-relative:\n{text}");
}

/// What the compiler makes of that source as an object, and what it said.
fn object(what: &str, source: &str, flags: &[&str]) -> (bool, Vec<u8>, String) {
    let path = fixture(what, source);
    let out = path.with_extension("o");
    let ran = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(flags)
        .arg("-c")
        .arg("-o")
        .arg(&out)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let bytes = std::fs::read(&out).unwrap_or_default();
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (ran.status.success(), bytes, String::from_utf8_lossy(&ran.stderr).into_owned())
}

#[test]
fn a_template_kept_as_text_names_a_local_by_where_it_is_in_the_frame() {
    // tcc's `asm_pcrel_test`. The template counts between two labels of its own, which nothing
    // here reads into instructions, so it is kept as text. The object it stores to is a local, and
    // is named by its place in the frame rather than by a register its address was put in, since
    // the next statement writes `%ecx` without saying so and gcc's listing survives that.
    let source = "unsigned f(void) { unsigned o; \
                  asm (\"1: mov $2f-1b,%%eax; mov %%eax,%0\\n2:\" : \"=m\" (o)); return o; }\n";
    let f = body(&asm("kept-local", source), "f");
    assert!(f.contains("1: mov $2f-1b,%eax; mov %eax,"), "{f}");
    assert!(f.contains("(%rsp)\n") || f.contains("(%rbp)\n"), "{f}");
}

#[test]
fn a_template_kept_as_text_spells_a_constant_and_a_name_the_way_gcc_does() {
    // The Linux kernel's bug table, which is what tcc's `get_asm_string` copies. `%c0` is the
    // operand without the `$` gcc would put in front of a constant, and the operand is the address
    // of a string, so it is the string's name.
    let source = "void f(void) { asm volatile (\".pushsection .data\\n.long %c0, %1, %c1\\n\
                  .popsection\" : : \"i\" (\"A string\"), \"i\" (7)); }\n";
    let f = body(&asm("kept-names", source), "f");
    assert!(f.contains("\t.long .L"), "{f}");
    assert!(f.contains(", $7, 7\n"), "{f}");
}

#[test]
fn a_true_bool_constant_is_spelled_one() {
    // The kernel's `arch_static_branch` writes `.quad %c0 + %c1 - .` with a `bool` branch. A one
    // bit true read as signed is `-1`, which put every likely branch's jump table entry one byte
    // before its key, and the kernel wrote over the key after it when it set up the table.
    let source = "struct k { int x; long t; } key;\n\
                  static inline __attribute__((always_inline)) _Bool br(struct k *const k, const _Bool b) {\n\
                  asm goto (\".pushsection __jt,\\\"aw\\\"\\n.quad %c0 + %c1 - .\\n.popsection\" \
                  : : \"i\" (k), \"i\" (b) : : yes);\n return 0;\nyes:\n return 1;\n}\n\
                  int f(void) { return br(&key, 1); }\n\
                  int g(void) { return br(&key, 0); }\n";
    let text = asm("bool-one", source);
    assert!(text.contains("key + 1 - ."), "{text}");
    assert!(text.contains("key + 0 - ."), "{text}");
    assert!(!text.contains("key + -1"), "{text}");
}

#[test]
fn a_template_kept_as_text_spells_a_register_operand_as_the_register_it_was_given() {
    // The jump with no condition is what keeps it as text. The input and the output are in
    // registers the allocator chose, which it only chose after the text was written down, so each
    // is spelled at the width of its type once it has one.
    let source = "int f(int x) { int r; asm (\"mov %1,%0; jmp 1f; 1:\" : \"=r\" (r) : \"r\" (x)); \
                  return r; }\n";
    let f = body(&asm("kept-register", source), "f");
    let line = f.lines().find(|line| line.contains("jmp 1f")).unwrap_or_default();
    assert!(line.trim_start().starts_with("mov %e") || line.contains("mov %r"), "{f}");
    assert_eq!(line.matches('%').count(), 2, "{f}");
    assert!(!f.contains('\u{1}'), "a hole was left in the text:\n{f}");
}

#[test]
fn a_register_operand_of_a_kept_template_is_spelled_at_the_width_its_modifier_asks_for() {
    // `+r` is one register read and written, and `%k0` of a `long` is its low four bytes. A pinned
    // operand is in the register its letter names.
    let source = "long f(long x) { asm (\"addl %k0,%k0; jmp 1f; 1:\" : \"+r\" (x)); return x; }\n\
                  int g(int x) { int r; asm (\"movb %h1,%b0; jmp 1f; 1:\" : \"=r\" (r) : \"a\" (x)); \
                  return r; }\n";
    let text = asm("kept-widths", source);
    let f = body(&text, "f");
    let line = f.lines().find(|line| line.contains("jmp 1f")).unwrap_or_default();
    let spelled: Vec<&str> = line.split([' ', ',', ';']).collect();
    assert!(spelled[1].starts_with("%e") || spelled[1].ends_with('d'), "{f}");
    assert_eq!(spelled[1], spelled[2], "one register read and written:\n{f}");
    let g = body(&text, "g");
    assert!(g.contains("movb %ah,%"), "{g}");
}

#[test]
fn an_output_of_a_kept_template_written_early_shares_no_register_with_an_input() {
    // `&` says the text writes the output before it has read every input, so the three inputs and
    // the output are four registers. Without it the output could have been given the first input's.
    let source = "int f(int a, int b, int c) { int r; \
                  asm (\"mov %1,%0; add %2,%0; add %3,%0; jmp 1f; 1:\" \
                  : \"=&r\" (r) : \"r\" (a), \"r\" (b), \"r\" (c)); return r; }\n";
    let f = body(&asm("kept-early", source), "f");
    let line = f.lines().find(|line| line.contains("jmp 1f")).unwrap_or_default();
    let named: Vec<&str> = line
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|word| word.starts_with('%'))
        .collect();
    assert_eq!(named.len(), 6, "{f}");
    let output = named[1];
    assert!(named[0] != output && named[2] != output && named[4] != output, "{f}");
}

#[test]
fn a_unit_with_a_template_kept_as_text_is_assembled_from_its_listing() {
    // A jump from one statement to a label another one defines, which only an assembler reading
    // the whole unit can resolve.
    let source = "int f(void) { int r; asm (\".text; jmp kept_there\"); \
                  asm (\"movl $1, %%eax\\nkept_there: movl $5, %%eax\" ::: \"eax\"); \
                  asm (\"mov %%eax,%0\" : \"=m\" (r)); return r; }\n";
    let (ok, bytes, said) = object("kept-object", source, &[]);
    assert!(ok, "{said}");
    assert!(bytes.windows(10).any(|at| at == b"kept_there"), "the label never reached the object");
}

#[test]
fn a_template_kept_as_text_that_reaches_nothing_past_itself_builds_with_debug_information() {
    // The label and the jump to it are both inside the one template, and what it names outside
    // itself is a variable the linker places, so the assembler reads it on its own where it is and
    // the rest of the unit keeps its line table.
    let source = "int kept_data; long f(long a, long b) { long r; \
                  asm (\"lea kept_data(%%rip), %0; add %1, %0; add %2, %0; jmp 1f; 1:\" \
                  : \"=&r\" (r) : \"r\" (a), \"r\" (b)); return r; }\n";
    let (ok, bytes, said) = object("kept-alone-debug", source, &["-g"]);
    assert!(ok, "{said}");
    assert!(bytes.windows(11).any(|at| at == b".debug_line"), "no line table in the object");
    assert!(bytes.windows(9).any(|at| at == b"kept_data"), "the variable never reached the object");
}

#[test]
fn a_unit_with_a_template_kept_as_text_builds_with_debug_information() {
    // The whole unit goes through a listing, which gets a label in front of every instruction when
    // `-g` is given, and the line table is built from where those labels land.
    let source = "void f(void) { asm volatile (\"jmp .Lgone\\n.Lgone:\"); }\n";
    let (ok, bytes, said) = object("kept-debug", source, &["-g"]);
    assert!(ok, "{said}");
    assert!(bytes.windows(11).any(|at| at == b".debug_line"), "no line table in the object");
    assert!(!bytes.windows(8).any(|at| at == b"rucc_row"), "a row label reached the object");
}

#[test]
fn a_constant_expression_given_as_n_is_spelled_as_a_number_without_optimisation() {
    // xz's range decoder hands `RC_BIT_MODEL_OFFSET`, a sum of shifts, to an `n` operand and writes
    // it as `%c[...]` in front of an address. Without optimisation the sum is arithmetic like any
    // other, so it has to be folded before the back end sees it or it arrives in a register.
    let source = "long f(long p) { long r; \
                  asm (\"lea %c1(%2), %0\\n1:\" : \"=r\" (r) : \"n\" ((1U << 5) - 1 - (1U << 11)), \
                  \"r\" (p)); return r; }\n";
    let text = asm("folded-n", source);
    assert!(body(&text, "f").contains("-2017("), "{text}");
}

#[test]
fn ten_outputs_written_plus_and_early_fit_in_the_registers_there_are() {
    // `+&` is one register shared with the input, the same as `+`, and ten of them with one more
    // input is what xz's range decoder hands a single template. Taking each as a register of its
    // own and a copy ran the allocator out of registers.
    let mut source = String::from("unsigned f(unsigned *p) {\n");
    for n in 0..10 {
        source.push_str(&format!("unsigned a{n} = p[{n}];\n"));
    }
    let outputs: Vec<String> = (0..10).map(|n| format!("\"+&r\" (a{n})")).collect();
    source.push_str(&format!("asm (\"1:\" : {} : \"r\" (p));\n", outputs.join(", ")));
    let sum: Vec<String> = (0..10).map(|n| format!("a{n}")).collect();
    source.push_str(&format!("return {}; }}\n", sum.join(" + ")));
    asm("plus-early", &source);
}

#[test]
fn a_clobber_list_may_name_the_vector_registers_the_template_uses() {
    // busybox's libbb/bitops.c, which every x86-64 build of it takes because SSE is always there.
    let text = asm(
        "xmm-clobber",
        "void f(void *dst, const void *src) {\n\
         asm volatile(\"movups (%0),%%xmm0\\n\\tmovups (%1),%%xmm1\\n\\txorps %%xmm1,%%xmm0\\n\\t\
         movups %%xmm0,(%0)\" : \"=r\" (dst), \"=r\" (src) : \"0\" (dst), \"1\" (src)\n\
         : \"xmm0\", \"%ymm1\", \"memory\"); }\n",
    );
    assert!(body(&text, "f").contains("xorps"), "{text}");
}

#[test]
fn a_vector_register_there_is_none_of_without_avx512_is_still_refused() {
    for name in ["xmm16", "zmm0", "xmm01"] {
        let source = format!("void f(void) {{ asm volatile(\"nop\" : : : \"{name}\"); }}\n");
        let (ok, _, said) = run("xmm-none", &source);
        assert!(!ok, "{name} was taken");
        assert!(said.contains("no name for"), "{said}");
    }
}

#[test]
fn eight_early_outputs_and_two_pinned_inputs_fit_with_optimisation() {
    // libsodium's `sodium_sub` in `utils.c`, which has eight `=&r` outputs and its two pointers in
    // `S` and `D`. With optimisation the allocator holds two registers back for itself, and the
    // count of what was left for the operands took them as free, so it ran out and panicked. The
    // loop after it is part of the shape, since without it nothing is spilled around the template.
    let mut source =
        String::from("void f(unsigned char *a, const unsigned char *b, unsigned long len) {\n");
    source.push_str("unsigned long t1, t2, t3, t4, t5, t6, t7, t8, i, c = 0;\nif (len == 64) {\n");
    let mut template = String::new();
    for n in 1..=8 {
        template.push_str(&format!("movq {}(%[in]), %[t{n}]\\n", (n - 1) * 8));
    }
    template.push_str("subq %[t1], (%[out])\\n");
    for n in 2..=8 {
        template.push_str(&format!("sbbq %[t{n}], {}(%[out])\\n", (n - 1) * 8));
    }
    let outputs: Vec<String> = (1..=8).map(|n| format!("[t{n}] \"=&r\" (t{n})")).collect();
    source.push_str(&format!(
        "__asm__ __volatile__(\"{template}\" : {} : [in] \"S\" (b), [out] \"D\" (a) \
         : \"memory\", \"flags\", \"cc\");\nreturn;\n}}\n\
         for (i = 0; i < len; i++) {{\n\
         c = (unsigned long) a[i] - (unsigned long) b[i] - c;\n\
         a[i] = (unsigned char) c;\n\
         c = (c >> 8) & 1;\n\
         }}\n}}\n",
        outputs.join(", ")
    ));
    for level in ["-O0", "-O1", "-O2", "-Os"] {
        let (ok, _, said) = object("sodium-sub", &source, &[level]);
        assert!(ok, "{level}:\n{said}");
    }
}

#[test]
fn a_macro_one_asm_defines_is_expanded_in_the_templates_after_it() {
    // The shapes behind the kernel's `ANNOTATE` and `_ASM_EXTABLE_TYPE_REG`. The first is a macro
    // defined by an `asm` at file scope and called from a template in a function further down. The
    // second is defined, called and purged in one template, and picks the number of the register
    // the output was given by comparing its name against each one in turn. The output is pinned so
    // that the number is known here: `%edx` is register 2, so the entry is `3 + (2 << 8)`.
    let source = r#"asm(".macro ANNOTATE type:req\n.Lhere_\\@:\n"
    ".pushsection .discard.annotate_insn,\"M\",@progbits,8\n"
    ".long .Lhere_\\@ - .\n.long \\type\n.popsection\n.endm\n");
int f(int *p) {
    int v;
    asm volatile("1: movl (%1), %0\n2:\n"
        ".pushsection __ex_table, \"a\"\n.balign 4\n.long 1b - .\n.long 2b - .\n"
        ".macro extable_type_reg type:req reg:req\n.set .Lfound, 0\n.set .Lregnr, 0\n"
        ".irp rs,eax,ecx,edx,ebx\n.ifc \\reg, %%\\rs\n.set .Lfound, .Lfound+1\n"
        ".long \\type + (.Lregnr << 8)\n.endif\n.set .Lregnr, .Lregnr+1\n.endr\n"
        ".if (.Lfound != 1)\n.error \"extable_type_reg: bad register argument\"\n.endif\n"
        ".endm\n"
        "extable_type_reg reg=%0, type=3\n.purgem extable_type_reg\n.popsection\n"
        : "=d"(v) : "r"(p));
    asm volatile("ANNOTATE 0x2269\n nop");
    return v;
}
"#;
    let (ok, bytes, said) = object("macros", source, &["-O1"]);
    assert!(ok, "{said}");
    let has = |what: &[u8]| bytes.windows(what.len()).any(|at| at == what);
    assert!(has(b"__ex_table"), "the exception table never reached the object");
    assert!(has(b".discard.annotate_insn"), "the annotation never reached the object");
    assert!(has(&[0x03, 0x02, 0, 0]), "the entry does not name the register the output is in");
    assert!(has(&[0x69, 0x22, 0, 0]), "the macro from file scope was not expanded");
}

/// An `asm goto` in each of the three shapes that used to be turned down: one with an output read
/// on the edge to its label, one in a function with a variable length array that leaves the scope
/// of the array every time round a loop, and one that leaves a block owing a cleanup handler. Each
/// answer is folded into the exit status, so a wrong one says which it was.
const GOTO_SHAPES: &str = r#"
static int freed;
__attribute__((noinline)) static void done(int *p) { freed += *p; }
__attribute__((noinline)) int out(int x) {
    int v;
    asm goto("testl %1, %1\n\tmovl $7, %0\n\tjz %l2" : "=r"(v) : "r"(x) : : zero);
    return v;
zero:
    return v + 100;
}
__attribute__((noinline)) int vla(int n, int x) {
    int total = 0;
    for (int i = 0; i < 3; i++) {
        int a[n];
        a[0] = i;
        asm goto("testl %0, %0\n\tjz %l1" : : "r"(x) : : again);
        total += a[0];
        continue;
    again:
        total += 10;
    }
    return total;
}
__attribute__((noinline)) int cleanup(int x) {
    {
        __attribute__((cleanup(done))) int k = 5;
        asm goto("testl %0, %0\n\tjz %l1" : : "r"(x) : : away);
        freed += 1000;
    }
    return freed;
away:
    return -freed;
}
int main(void) {
    if (out(0) != 107 || out(1) != 7) return 1;
    if (vla(4, 0) != 30 || vla(4, 1) != 3) return 2;
    if (cleanup(0) != -5 || cleanup(1) != 1010) return 3;
    return 42;
}
"#;

#[test]
fn an_asm_goto_leaving_a_cleanup_handler_runs_it_on_the_way_to_the_label() {
    // The handler is called once on each way out of the block, and the edge to the label is one of
    // them, so it is in the listing twice.
    for level in ["-O0", "-O2"] {
        let (ok, text, said) = run_with(&format!("goto-shapes{level}"), GOTO_SHAPES, &[level]);
        assert!(ok, "the compiler refused the fixture at {level}:\n{said}");
        let calls = body(&text, "cleanup").matches("call\tdone").count();
        assert_eq!(calls, 2, "at {level}:\n{}", body(&text, "cleanup"));
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn an_asm_goto_with_an_output_an_array_or_a_handler_gives_the_right_answers() {
    for level in ["-O0", "-O1", "-O2", "-O3"] {
        let path = fixture(&format!("goto-run{level}"), GOTO_SHAPES);
        let prog = path.with_extension("");
        let built = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .arg(level)
            .arg("-o")
            .arg(&prog)
            .arg(&path)
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&prog).output().expect("what was linked can be run");
        let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
        assert_eq!(ran.status.code(), Some(42), "a wrong answer at {level}");
    }
}

/// An `"i"` operand that is the address of a member of a static is the static's name plus the
/// member's offset, spelled the way gcc spells it. The kernel's dynamic debug passes one of those
/// to the jump label's `asm goto`, which writes it into `__jump_table` with `%c0`.
#[test]
fn an_immediate_that_is_a_name_plus_an_offset_is_spelled_as_both() {
    let source = "\
struct key { int enabled; };
struct entry { const char *format; long flags; struct key key; };
static struct entry entry;
void *before;
int on(void) {
    asm goto(\"1: jmp %l[yes]\\n\\t.pushsection __jump_table, \\\"aw\\\"\\n\\t\"
             \".quad %c0 + %c1 - .\\n\\t.quad %c2\\n\\t.popsection\"
             : : \"i\" (&entry.key), \"i\" (2), \"i\" ((char *)&before - 8) : : yes);
    return 0;
yes:
    return 1;
}
";
    let text = asm("symbol-offset", source);
    assert!(text.contains(".quad entry+16 + 2 - ."), "{text}");
    assert!(text.contains(".quad before-8"), "{text}");
}

/// A flag output is the condition read into a register after the template, the way the kernel's
/// `CC_SET` asks for it: `setc` into a byte for a `_Bool`, and a wider output cleared above it.
#[test]
fn a_flag_output_is_the_condition_read_after_the_template() {
    let source = "\
int bit(const unsigned long *p, long n) {
    _Bool c;
    asm(\"btq %2, %1\" : \"=@ccc\" (c) : \"m\" (*p), \"r\" (n));
    return c;
}
int zero(int *v) {
    int z;
    asm volatile(\"decl %0\" : \"+m\" (*v), \"=@ccz\" (z));
    return z;
}
";
    let text = asm("flag-output", source);
    assert!(text.contains("setb") || text.contains("setc"), "{text}");
    assert!(text.contains("sete") || text.contains("setz"), "{text}");
    assert!(text.contains("movzbl"), "{text}");
    let (ok, _, said) =
        run("flag-output-bad", "int f(void) { int z; asm(\"\" : \"=@ccq\" (z)); return z; }");
    assert!(!ok && said.contains("flag output"), "{said}");
}

/// A template kept as text that names two operands in memory, which lib/raid6 writes to load one
/// block and fetch the next in the same statement. One is the instruction's own memory operand and
/// the other is read through the register its address is in.
#[test]
fn a_kept_template_naming_two_operands_in_memory_reaches_both() {
    let source = "\
void f(unsigned char *p, unsigned char *q) {
    asm volatile(\"prefetchnta %0\\n\\tvmovdqa64 %0,%%zmm2\\n\\tvmovntdq %%zmm2,%1\"
                 : : \"m\" (*p), \"m\" (*q));
}
";
    let text = asm("two-in-memory", source);
    let body = body(&text, "f");
    assert!(body.contains("prefetchnta (%rdi)"), "{body}");
    assert!(body.contains("vmovdqa64 (%rdi),%zmm2"), "{body}");
    assert!(body.contains("vmovntdq %zmm2,(%rsi)"), "{body}");
}

/// A statement with no operands and no clobbers written with the colons anyway is extended
/// assembly, whose `%%` is one `%`. Basic assembly would hand `%%zmm5` to the assembler as it is,
/// which no assembler takes, so a template with `%%` in it was written with them.
#[test]
fn a_template_with_empty_colons_and_a_doubled_percent_is_extended() {
    let source = "void f(void) { asm volatile(\"vpxorq %%zmm5,%%zmm5,%%zmm5\" : : ); }\n";
    let text = asm("empty-colons", source);
    let body = body(&text, "f");
    assert!(body.contains("vpxorq %zmm5,%zmm5,%zmm5"), "{body}");
}

#[test]
fn a_width_modifier_on_a_constant_prints_the_constant() {
    // The kernel's `outb`, with the port known. `%w1` asks for the word register the port is in,
    // and when `"Nd"` made it a constant there is none, so gcc writes the number.
    let (ok, text, said) = run_with(
        "width-on-constant",
        "void f(unsigned char v) { asm volatile(\"outb %b0, %w1\" : : \"a\"(v), \"Nd\"((unsigned short)0x21)); }\n",
        &["-O2"],
    );
    assert!(ok, "{said}");
    assert!(text.contains("outb %al, $33"), "{text}");
}

#[test]
fn a_constant_outside_its_range_letter_goes_in_the_register_the_constraint_also_allows() {
    // `N` is a port from 0 to 255, so the kernel's `outb` to 0x4d0 needs the `d` half of `"Nd"`.
    let (ok, text, said) = run_with(
        "outside-range",
        "void f(unsigned char v) { asm volatile(\"outb %b0, %w1\" : : \"a\"(v), \"Nd\"((unsigned short)0x4d0)); }\n",
        &["-O2"],
    );
    assert!(ok, "{said}");
    assert!(text.contains("movw\t$1232, %dx"), "{text}");
    assert!(text.contains("outb %al, %dx"), "{text}");
}

#[test]
fn a_constant_wider_than_thirty_two_bits_goes_in_a_register_under_e_and_z() {
    // The kernel's `atomic64_add` is `"er"`, and `addq` has room for a sign extended thirty two
    // bit number and no more, so lib/atomic64_test.c adding 0x1111111111111122 needs the `r`.
    let source = "void f(long *p) {\n\
                  asm volatile(\"lock; addq %1,%0\" : \"+m\"(*p) : \"er\"(0x1111111111111122L));\n\
                  asm volatile(\"lock; addq %1,%0\" : \"+m\"(*p) : \"er\"(-1L));\n\
                  asm volatile(\"movl %1,%k0\" : \"=r\"(*p) : \"Zr\"(0xffffffffL));\n\
                  asm volatile(\"movl %1,%k0\" : \"=r\"(*p) : \"Zr\"(-1L)); }\n";
    let (ok, text, said) = run_with("e-and-z", source, &["-O2"]);
    assert!(ok, "{said}");
    assert!(!text.contains("addq $1229782938247303458"), "{text}");
    assert!(text.contains("lock; addq %rax,"), "{text}");
    assert!(text.contains("lock; addq $-1,"), "{text}");
    assert!(text.contains("movl $4294967295,"), "{text}");
    assert!(!text.contains("movl $-1,"), "{text}");
}

#[test]
fn an_address_turned_into_a_number_and_back_is_still_a_constant() {
    // The kernel's gdt_idt.c hands `rip_rel_ptr` a per cpu variable as
    // `(void *)(unsigned long)&gdt_page`, and the template asks for it with `"i"`.
    let source = "extern char page[4096];\n\
                  static inline __attribute__((always_inline)) void *rel(void *p) {\n\
                  asm(\"leaq %c1(%%rip), %0\" : \"=r\"(p) : \"i\"(p)); return p; }\n\
                  void *f(void) { return rel((void *)(unsigned long)&page[8]); }\n";
    for level in ["-O0", "-O2"] {
        let (ok, text, said) = run_with("cast-address", source, &[level]);
        assert!(ok, "{level}: {said}");
        assert!(text.contains("leaq page+8(%rip)"), "{level}: {text}");
    }
}
