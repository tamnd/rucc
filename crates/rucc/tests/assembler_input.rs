//! A file of assembly on the command line, end to end.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1.
//!
//! A `.s` or a `.S` next to the C is ordinary in a project with a hot loop in it, and until there
//! was an assembler what this file checked was that the compiler said so rather than exiting zero
//! having written nothing. Now there is one, so what is checked is the other side of the same
//! question: a file of directives produces a real object, a function written in assembly links and
//! gives the right answer when it runs, a mnemonic this compiler has no bytes for is still refused
//! by name and by line, and none of them exits zero with nothing beside it.
//!
//! GMP is the project that showed it, and it is the case in `gmp_thirty_two_bit_word_probe` below.
//! Its configure assembles a file with no instruction in it to find out how the local assembler
//! spells a thirty two bit word, and reads the answer out of the value of a symbol in the object, so
//! nothing but an object will do. Before there was one it found no object beside any of its three
//! probes and gave up with `cannot determine how to define a 32-bit word`.
//!
//! What is not checked here is the shape of the object down to the byte. `rucc-object` and
//! `rucc-asm` have their own tests for that, and the comparison that matters is against what gas
//! writes for the same input, which is a corpus run and not a unit test. What is here is the
//! driver's half: the right file appears, with the right name, in the right place.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The target is written down rather than taken from the host, so this is the same question on
/// every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// What GMP's probe writes, which has no instruction in it at all: a section, a name, a label and
/// four bytes.
const ASSEMBLY: &str = "\t.text\n\t.globl probe\nprobe:\n\t.long 0\n";

/// A directory of its own, so that two of these running at once do not write the same file.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-asm-input-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// One file written under it.
fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("the fixture can be written");
    path
}

/// Whether the compiler finished, and what it said.
fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Whether a file is a relocatable ELF object.
///
/// The header and nothing past it. Sixteen bytes of identification, then two bytes of type, and
/// type one is the kind a compiler writes. Checking this much is what says the file is an object
/// rather than a copy of its own input, which is the mistake a driver that forwarded the wrong
/// temporary would make and which a test that only asked whether the name exists would miss.
fn is_an_object(path: &Path) -> bool {
    let bytes = std::fs::read(path).expect("the object can be read back");
    bytes.len() > 18 && &bytes[..4] == b"\x7fELF" && bytes[16] == 1 && bytes[17] == 0
}

#[test]
fn gmp_thirty_two_bit_word_probe() {
    let dir = dir("alone");
    write(&dir, "probe.s", ASSEMBLY);
    let (ok, said) = run(&dir, &["-c", "probe.s"]);
    assert!(ok, "the probe was refused:\n{said}");
    let object = dir.join("probe.o");
    assert!(object.exists(), "no object appeared beside the input");
    assert!(is_an_object(&object), "what appeared is not a relocatable object");
    // The name the probe defines has to be in the file, because the value beside it is the whole
    // answer configure is after. It is in the string table, so it is in the bytes.
    let bytes = std::fs::read(&object).expect("the object can be read back");
    assert!(bytes.windows(5).any(|w| w == b"probe"), "the object does not name what it defines");
}

#[test]
fn naming_the_output_puts_it_there() {
    // Because the rule that derives a name from an input and an explicit `-o` are two paths, and a
    // check written against one of them would pass while the other wrote somewhere else.
    let dir = dir("named");
    write(&dir, "probe.s", ASSEMBLY);
    let (ok, said) = run(&dir, &["-c", "probe.s", "-o", "elsewhere.o"]);
    assert!(ok, "the probe was refused:\n{said}");
    assert!(is_an_object(&dir.join("elsewhere.o")), "the object is not where it was asked for");
    assert!(
        !dir.join("probe.o").exists(),
        "it was also written to the name that was not asked for"
    );
}

#[test]
fn assembly_that_wants_the_preprocessor_first_goes_through_it() {
    // A `.S` runs through the preprocessor and then needs assembling, so it has one more phase in
    // front of it. Separate because the two kinds are separate in the plan, and the macro here is
    // the point: a driver that skipped the preprocessor would hand `.long ZERO` to a reader that
    // has never heard of `ZERO` and the file would be refused rather than quietly wrong.
    let dir = dir("cpp");
    let text = "#define ZERO 0\n\t.text\n\t.globl probe\nprobe:\n\t.long ZERO\n";
    write(&dir, "probe.S", text);
    let (ok, said) = run(&dir, &["-c", "probe.S"]);
    assert!(ok, "the probe was refused:\n{said}");
    assert!(is_an_object(&dir.join("probe.o")), "no object appeared beside the input");
}

#[test]
fn a_function_written_in_assembly_is_compiled_and_called() {
    // The other half, end to end and run rather than inspected, because an object whose bytes are
    // wrong links exactly as well as one whose bytes are right. The function is the one every
    // hand written file starts with, and the answer it gives is the one thing that says the bytes
    // came out as the instructions and not as something that merely had the right length.
    let dir = dir("instruction");
    let hot = "\t.text\n\t.globl triple\n\t.type triple, @function\ntriple:\n\tmovl %edi, \
               %eax\n\taddl %edi, %eax\n\taddl %edi, %eax\n\tret\n\t.size triple, .-triple\n";
    write(&dir, "hot.s", hot);
    write(&dir, "main.c", "extern int triple(int);\nint main(void) { return triple(14); }\n");
    let (ok, said) = run(&dir, &["main.c", "hot.s", "-o", "prog"]);
    assert!(ok, "the link failed:\n{said}");
    let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
    assert_eq!(out.status.code(), Some(42), "the assembly did not do what it says");
}

#[test]
fn a_loop_written_the_way_a_hand_written_library_writes_one_gives_the_right_answer() {
    // The instructions a C expression never compiles to, run rather than inspected. This is the
    // shape of GMP's `mpn_add_n` reduced to what fits in a test: a carry carried from one addition
    // to the next by `adc`, a counter stepped by `dec` because `dec` leaves the carry alone where
    // an addition of one would destroy it, and `jrcxz` deciding whether there is anything to do.
    // Every one of those was refused until the encoder had rows for them, and the reason they are
    // worth running is that each of them encodes to something on its own: a wrong row here is a
    // real instruction doing the wrong arithmetic, which links and which nothing but the answer
    // catches.
    //
    // The exclusive or at the top is there to clear the carry as well as the answer, which is the
    // idiom the real thing uses, and `lea` is here because it is the addition that does not touch
    // the flags and so is the only way to walk three pointers between two `adc` instructions.
    let dir = dir("carry");
    let hot = "\t.text\n\t.globl addn\n\t.type addn, @function\naddn:\n\txorl %eax, \
               %eax\n\tjrcxz done\nover:\n\tmovq (%rsi), %r8\n\tadcq (%rdx), %r8\n\tmovq %r8, \
               (%rdi)\n\tleaq 8(%rsi), %rsi\n\tleaq 8(%rdx), %rdx\n\tleaq 8(%rdi), \
               %rdi\n\tdecq %rcx\n\tjnz over\n\tsetc %al\ndone:\n\tret\n\t.size addn, .-addn\n";
    write(&dir, "hot.s", hot);
    // Two limbs of all ones on top of one each, so the carry comes out of the first addition, goes
    // into the second, comes out of that one too and lands in the third. A missing carry gives a
    // different answer in every limb but the first.
    let main = "#include <stdint.h>\nextern int addn(uint64_t *r, const uint64_t *a, const \
                uint64_t *b, unsigned long n);\nint main(void) {\n  uint64_t a[3] = \
                {0xffffffffffffffffUL, 0xffffffffffffffffUL, 1};\n  uint64_t b[3] = {1, 1, 1};\n  \
                uint64_t r[3] = {9, 9, 9};\n  int carry = addn(r, a, b, 3);\n  if (r[0] != 0) \
                return 1;\n  if (r[1] != 1) return 2;\n  if (r[2] != 3) return 3;\n  if (carry != \
                0) return 4;\n  if (addn(r, a, b, 0) != 0) return 5;\n  return 42;\n}\n";
    write(&dir, "main.c", main);
    let (ok, said) = run(&dir, &["main.c", "hot.s", "-o", "prog"]);
    assert!(ok, "the link failed:\n{said}");
    let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
    assert_eq!(out.status.code(), Some(42), "the assembly did not add the way it says");
}

#[test]
fn the_shapes_a_hand_written_library_reaches_memory_with_give_the_right_answers() {
    // The second half of the same idea. Every instruction here is one the encoder had no row for
    // until it was asked to read somebody else's file, and each of them is wrong in a different
    // way if the row is wrong, so the answers are what tells them apart.
    //
    // `carryadd` adds into memory without loading first and carries up the array, which is the
    // only shape of the eight that had no row at all, and it adds a constant to a place in memory
    // as well. `dshift` moves a window across two registers, which is what shifting a number wider
    // than a register is and is the instruction GMP writes every one of its shifting loops with.
    // `highmul` multiplies straight out of memory. `halve` rotates one place through the carry,
    // which is a third opcode rather than the constant one written short, with `clc` and `stc`
    // putting the carry where it has to be first. `sumneg` counts a negative index up to zero and
    // ends on `js`, which is the idiom every GMP loop uses and is one of the six conditions a C
    // expression has no way to ask about.
    let dir = dir("memory");
    let hot = "\t.text\n\t.globl carryadd\n\t.type carryadd, @function\ncarryadd:\n\txorl %eax, \
               %eax\n\taddq %rsi, (%rdi)\n\tjnc done\n\tmovl $1, %ecx\nup:\n\tcmpq %rdx, \
               %rcx\n\tjae out\n\taddq $1, (%rdi,%rcx,8)\n\tjnc done\n\taddq $1, %rcx\n\tjmp \
               up\nout:\n\tmovl $1, %eax\ndone:\n\tret\n\t.size carryadd, .-carryadd\n\t.globl \
               dshift\n\t.type dshift, @function\ndshift:\n\tmovq %rdx, %rcx\n\tmovq %rdi, \
               %rax\n\tshldq %cl, %rsi, %rax\n\tret\n\t.size dshift, .-dshift\n\t.globl \
               highmul\n\t.type highmul, @function\nhighmul:\n\tmovq %rsi, %rax\n\tmulq \
               (%rdi)\n\tmovq %rdx, %rax\n\tret\n\t.size highmul, .-highmul\n\t.globl \
               halve\n\t.type halve, @function\nhalve:\n\tmovq %rdi, %rax\n\tclc\n\ttestl %esi, \
               %esi\n\tje noc\n\tstc\nnoc:\n\trcrq %rax\n\tret\n\t.size halve, .-halve\n\t.globl \
               sumneg\n\t.type sumneg, @function\nsumneg:\n\txorl %eax, %eax\n\tleaq \
               (%rdi,%rsi,8), %rdi\n\tnegq %rsi\n\tjz none\nback:\n\taddq (%rdi,%rsi,8), \
               %rax\n\taddq $1, %rsi\n\tjs back\nnone:\n\tret\n\t.size sumneg, .-sumneg\n";
    write(&dir, "hot.s", hot);
    // Two limbs of all ones under a zero, so a one added at the bottom carries the whole way up and
    // lands in the third, and the same thing in an array of one limb so the carry runs off the end.
    let main = "#include <stdint.h>\nextern int carryadd(uint64_t *r, uint64_t v, long n);\nextern \
                uint64_t dshift(uint64_t hi, uint64_t lo, long cnt);\nextern uint64_t highmul(const \
                uint64_t *p, uint64_t v);\nextern uint64_t halve(uint64_t x, int carry);\nextern \
                long sumneg(const long *p, long n);\nint main(void) {\n  uint64_t r[3] = \
                {0xffffffffffffffffUL, 0xffffffffffffffffUL, 0};\n  if (carryadd(r, 1, 3) != 0) \
                return 1;\n  if (r[0] != 0) return 2;\n  if (r[1] != 0) return 3;\n  if (r[2] != 1) \
                return 4;\n  uint64_t f[1] = {0xffffffffffffffffUL};\n  if (carryadd(f, 1, 1) != 1) \
                return 5;\n  if (f[0] != 0) return 6;\n  if (dshift(0x1234UL, \
                0x8000000000000000UL, 4) != 0x12348UL) return 7;\n  uint64_t big = \
                0xffffffffffffffffUL;\n  if (highmul(&big, 2) != 1) return 8;\n  if (halve(4, 0) != \
                2) return 9;\n  if (halve(4, 1) != (0x8000000000000000UL | 2)) return 10;\n  long \
                p[4] = {1, 2, 3, 4};\n  if (sumneg(p, 4) != 10) return 11;\n  if (sumneg(p, 0) != \
                0) return 12;\n  return 42;\n}\n";
    write(&dir, "main.c", main);
    let (ok, said) = run(&dir, &["main.c", "hot.s", "-o", "prog"]);
    assert!(ok, "the link failed:\n{said}");
    let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
    assert_eq!(out.status.code(), Some(42), "the assembly did not do what it says");
}

#[test]
fn an_instruction_this_compiler_has_no_bytes_for_is_refused_by_name_and_by_line() {
    // Refusing it is the whole design of the reader: an assembler that skipped what it did not
    // recognise would write an object that links, and what would be wrong with it is a run of
    // missing bytes in the middle of a function, which nothing finds until the program runs.
    let dir = dir("unwritten");
    write(&dir, "hot.s", "\t.text\n\t.globl go\ngo:\n\tbswap %rax\n\tret\n");
    let (ok, said) = run(&dir, &["-c", "hot.s"]);
    assert!(!ok, "an instruction with no bytes behind it was accepted:\n{said}");
    assert!(said.contains("bswap"), "the message does not name the instruction:\n{said}");
    assert!(said.contains("hot.s:4"), "the message does not carry the line:\n{said}");
    assert!(!dir.join("hot.o").exists(), "a half-written object was left behind");
}

#[test]
fn a_link_takes_the_object_the_assembly_produced() {
    // The driver schedules an object for the assembly in a temporary directory and hands the path
    // to the linker, and until there was an assembler the link was where it came apart, with `ld`
    // saying it cannot find a file in a directory nobody named. The variable rather than a function
    // because a label and four bytes are not a function body, and what is being checked is that the
    // linker was given something it could resolve a name out of.
    let dir = dir("link");
    write(&dir, "main.c", "extern int probe_value;\nint main(void) { return probe_value; }\n");
    let value = "\t.data\n\t.globl probe_value\n\t.type probe_value, @object\n\t.size probe_value, \
                 4\nprobe_value:\n\t.long 7\n";
    write(&dir, "probe.s", value);
    let (ok, said) = run(&dir, &["main.c", "probe.s", "-o", "both"]);
    assert!(ok, "the link failed:\n{said}");
    assert!(dir.join("both").exists(), "nothing was linked");
    assert!(!said.contains("cannot find"), "the linker was handed a path to nothing:\n{said}");
}

#[test]
fn an_object_file_is_not_this_and_is_still_passed_through() {
    // The check is on the phases rather than on the kind, and an object has no compile phase
    // either. It also has no assemble phase, which is the difference, and a compiler that ran the
    // assembly reader over `a.o` would refuse a file that is already what it was asked to make.
    let dir = dir("object");
    write(&dir, "main.c", "int main(void) { return 0; }\n");
    let (ok, said) = run(&dir, &["-c", "main.c"]);
    assert!(ok, "the compiler refused the fixture:\n{said}");
    let (ok, said) = run(&dir, &["main.o", "-o", "prog"]);
    assert!(ok, "an object file was refused:\n{said}");
    assert!(dir.join("prog").exists(), "nothing was linked");
}

#[test]
fn stopping_before_the_assembler_writes_nothing_and_says_nothing() {
    // A `.s` has no preprocessing phase, so `rucc -E probe.s` asks for a phase the file does not
    // have and there is nothing left to do. gcc 16 exits zero and writes no file at all for the
    // same command, and this is here because the obvious reading of `-E` is that it copies the text
    // through, which would leave a file beside a build that never asked for one.
    let dir = dir("stopped");
    write(&dir, "probe.s", ASSEMBLY);
    let (ok, said) = run(&dir, &["-E", "probe.s", "-o", "out.i"]);
    assert!(ok, "a mode that never reaches the assembler was refused:\n{said}");
    assert!(!dir.join("out.i").exists(), "a file was written for a phase the input does not have");
    assert!(!dir.join("probe.o").exists(), "an object was written by a mode that stops before one");
}
