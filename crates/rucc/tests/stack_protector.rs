//! Which functions get a stack protector and what one looks like, end to end.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.6 and `spec/10-backend.md` section 10.7.
//!
//! Two questions and they are answered in two different crates, which is why they are checked
//! together here rather than apart in each. Which functions get one is a question about the locals
//! a function declares, so it is settled in the lowering while the types are still around. What one
//! is made of is a slot in the frame and a comparison before every return, so it is settled in the
//! back end after the allocator has finished. A test in either crate alone can be green while the
//! flag on the command line does nothing.
//!
//! The listing rather than the object, for the reason `pic.rs` beside this reads the listing: it is
//! what a person debugging this reads. That the bytes come out right is checked by the encoder's
//! own tests, which have the one address this feature adds written out in hex.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, because where the word a canary is
/// copied from lives is a fact about the platform and the answer elsewhere is a different one.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The four kinds of function the levels disagree about.
///
/// `leaf` has nothing in it at all, `buf` has an array big enough for anybody, `small` has one
/// under the size the plain flag asks for, and `taken` has no array but hands the address of a
/// local to something that could write through it.
const FOUR: &str = "\
void use(void *);
int leaf(int x) { return x + 1; }
int buf(void) { char b[16]; use(b); return 0; }
int small(void) { char b[4]; use(b); return 0; }
int taken(void) { int n = 0; use(&n); return n; }
";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-ssp-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Runs the compiler over that source under those flags, for that target.
fn run(what: &str, target: &str, flags: &[&str], source: &str) -> std::process::Output {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-S", "-o", "-"])
        // The frames read here are the ones taken without a frame pointer, which `-O0` would
        // otherwise keep as gcc does. A test that wants one says so after this and wins.
        .arg("-fomit-frame-pointer")
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    out
}

/// The assembly the compiler writes for that source under those flags.
fn asm(what: &str, flags: &[&str], source: &str) -> String {
    let out = run(what, TARGET, flags, source);
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Which of the four functions in the listing were given a canary.
///
/// Read off the listing rather than counted, because a count would be the same for two different
/// sets and the whole question here is which ones.
fn protected(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut current = None;
    for line in text.lines() {
        if let Some(name) = line.strip_suffix(':') {
            if !name.starts_with('.') {
                current = Some(name);
            }
        }
        if line.contains("%fs:40") {
            if let Some(name) = current.take() {
                out.push(name);
            }
        }
    }
    out
}

/// The plain flag protects a function with an array in it and nothing else.
#[test]
fn the_plain_flag_protects_the_functions_with_a_buffer_in_them() {
    let text = asm("plain", &["-fstack-protector"], FOUR);
    assert_eq!(protected(&text), ["buf"], "{text}");
}

/// The strong one adds the small array and the address that got away.
///
/// This is the one every distribution builds with, so it is the one worth being exact about. An
/// address-taken local is in because whatever it was handed to can write through it and nothing
/// here knows how far, which is the same argument the array makes.
#[test]
fn the_strong_flag_adds_the_small_arrays_and_the_locals_that_escape() {
    let text = asm("strong", &["-fstack-protector-strong"], FOUR);
    assert_eq!(protected(&text), ["buf", "small", "taken"], "{text}");
}

/// The last one protects everything, including a leaf with no memory in it at all.
#[test]
fn the_all_flag_protects_every_function_there_is() {
    let text = asm("all", &["-fstack-protector-all"], FOUR);
    assert_eq!(protected(&text), ["leaf", "buf", "small", "taken"], "{text}");
}

/// And nothing is protected unless something asked, which is gcc's default and this one.
#[test]
fn nothing_is_protected_unless_the_command_line_asked_for_it() {
    for flags in [&[][..], &["-fstack-protector-strong", "-fno-stack-protector"]] {
        let text = asm("off", flags, FOUR);
        assert_eq!(protected(&text), Vec::<&str>::new(), "{flags:?}: {text}");
    }
}

/// A function that says `no_stack_protector` gets none, whatever the flag asked for.
///
/// The kernel writes it on the code that runs before the canary is set up, and it is written on a
/// prototype there as often as on the definition. `__has_attribute` answers yes for it, so a header
/// that asks and then writes it is owed a function with no check on the way out.
#[test]
fn a_function_that_says_no_stack_protector_gets_none_under_any_flag() {
    let source = "\
void use(void *);
__attribute__((__no_stack_protector__)) int early(void);
int early(void) { char b[16]; use(b); return 0; }
__attribute__((no_stack_protector)) int late(void) { int n = 0; use(&n); return n; }
int buf(void) { char b[16]; use(b); return 0; }
";
    for flags in ["-fstack-protector", "-fstack-protector-strong", "-fstack-protector-all"] {
        let text = asm("refused", &[flags], source);
        assert_eq!(protected(&text), ["buf"], "{flags}: {text}");
    }
}

/// `optimize("no-stack-protector")` is the older way to say the same, in any spelling gcc takes.
///
/// It is what the kernel's `__nostackprotector` was before gcc 11 had the attribute, and what
/// `compiler_attributes.h` still falls back to on a compiler that does not have it.
#[test]
fn an_optimize_attribute_that_turns_the_protector_off_is_read_as_the_attribute() {
    let source = "\
void use(void *);
__attribute__((optimize(\"no-stack-protector\"))) int one(void) { char b[64]; use(b); return 0; }
__attribute__((optimize(\"-fno-stack-protector\"))) int two(void) { char b[64]; use(b); return 0; }
__attribute__((__optimize__(\"O2,no-stack-protector\"))) int three(void) { char b[64]; use(b); return 0; }
int buf(void) { char b[64]; use(b); return 0; }
";
    for flags in ["-fstack-protector", "-fstack-protector-strong", "-fstack-protector-all"] {
        let text = asm("optimize", &[flags], source);
        assert_eq!(protected(&text), ["buf"], "{flags}: {text}");
    }
}

/// `stack_protect` asks for a canary under any flag that turns protection on, even in a function
/// with nothing to protect, and `-fstack-protector-explicit` protects only the functions that ask.
/// With `-fno-stack-protector` it asks for nothing. Of `stack_protect` and `no_stack_protector` on
/// the same function, the one written first stands. All of it measured against gcc 13.
#[test]
fn a_function_that_says_stack_protect_gets_one_whenever_protection_is_on() {
    let source = "\
void use(void *);
__attribute__((stack_protect)) int asked(void) { return 1; }
__attribute__((stack_protect)) int declared(void);
int declared(void) { return 2; }
__attribute__((stack_protect, no_stack_protector)) int first(void) { return 3; }
__attribute__((no_stack_protector, stack_protect)) int second(void) { char b[64]; use(b); return 4; }
int buf(void) { char b[64]; use(b); return 0; }
";
    let text = asm("explicit-off", &["-fno-stack-protector"], source);
    assert_eq!(protected(&text), Vec::<&str>::new(), "{text}");
    let text = asm("explicit", &["-fstack-protector-explicit"], source);
    assert_eq!(protected(&text), ["asked", "declared", "first"], "{text}");
    for flags in ["-fstack-protector", "-fstack-protector-strong"] {
        let text = asm("explicit-on", &[flags], source);
        assert_eq!(protected(&text), ["asked", "declared", "first", "buf"], "{flags}: {text}");
    }
}

/// What a protected function is made of, in the order it is made of it.
///
/// The prologue reads the word out of the block the thread has to itself and puts a copy above
/// every byte a local reaches. Before the return it reads the word again and compares, and the arm
/// where the two differ calls the function that does not come back. The order matters more than
/// the mnemonics: a check written after the frame was given back would be checking a slot the
/// function no longer owns.
#[test]
fn a_protected_function_copies_the_word_and_compares_it_before_it_returns() {
    let text = asm("shape", &["-fstack-protector-strong"], FOUR);
    let body: Vec<&str> = text
        .lines()
        .skip_while(|line| !line.starts_with("buf:"))
        .take_while(|line| !line.starts_with("small:"))
        .map(str::trim)
        .collect();

    let at = |what: &str| {
        body.iter().position(|line| line.contains(what)).unwrap_or_else(|| panic!("{what}: {text}"))
    };
    // Two reads of the same address, one in the prologue and one before the return, with the store
    // into the frame behind the first of them.
    assert!(at("movq\t%fs:40") < at("__stack_chk_fail"), "{text}");
    assert!(at("__stack_chk_fail") < at("ret"), "{text}");
    assert_eq!(body.iter().filter(|line| line.contains("%fs:40")).count(), 2, "{text}");
    // And the frame is given back after the check rather than before it, so the slot the check
    // reads is still this function's when it reads it.
    assert!(at("__stack_chk_fail") < at("addq\t$"), "{text}");
}

/// A target whose protector is a different mechanism is told so rather than quietly left open.
///
/// Windows has one and it is not this one: the cookie is a global the loader writes, what goes in
/// the frame is that global exclusive-ored with the frame pointer, and the check is a call rather
/// than a comparison. Accepting the flag and emitting nothing would be the one outcome worse than
/// the error, because the build would look protected and not be.
#[test]
fn a_target_whose_protector_is_a_different_mechanism_refuses_the_flag() {
    let out = run("windows", "x86_64-pc-windows-msvc", &["-fstack-protector"], FOUR);
    assert!(!out.status.success(), "the flag does nothing on that target and it says so");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("-fstack-protector"), "{stderr}");
    // The triple as the compiler spells it back, which is the normalized one rather than the one
    // the command line wrote.
    assert!(stderr.contains("x86_64-windows-msvc"), "{stderr}");
}

/// A protected function whose only call is in tail position keeps it as a call.
///
/// The check has to run after the callee returns, since the callee may be what writes past the
/// canary, so the call cannot become a jump. The frame is not a leaf's, because a protected one
/// never is, and leaving the call alone is right. This used to trip an assertion in the back end
/// that expected every tail call in a leaf to have become a jump, and the kernel's
/// `kmalloc_array_noprof` was the function that found it. The canary is asked for by name, since
/// `bytes` becomes a value at `-O2` and would otherwise take it away.
#[test]
fn a_tail_call_in_a_protected_function_stays_a_call_before_the_check() {
    let source = "\
void *alloc(unsigned long size);
__attribute__((stack_protect)) void *alloc_array(unsigned long n, unsigned long size) {
    unsigned long bytes;
    if (__builtin_mul_overflow(n, size, &bytes))
        return 0;
    return alloc(bytes);
}
";
    let text = asm("tail", &["-O2", "-fstack-protector-strong"], source);
    let at = |what: &str| {
        text.lines()
            .position(|line| line.contains(what))
            .unwrap_or_else(|| panic!("{what}: {text}"))
    };
    assert!(at("call\talloc") < at("__stack_chk_fail"), "{text}");
    assert!(!text.contains("jmp\talloc"), "{text}");
}

/// A local that only gave the function its canary because of what the optimizer could not yet see
/// stops giving it one when the optimizer makes it a value.
///
/// gcc decides at expansion, after inlining and scalar replacement have run, so `gone` has no
/// canary there at `-O2` while `kept`, whose address goes somewhere nothing can follow, and `buf`,
/// whose array stays in the frame, both do. At `-O0` all three keep their locals in memory and all
/// three are protected. The kernel's `guard(rcu)()` and the `old` that `atomic_try_cmpxchg` is
/// handed are the shape of `gone`, and `mm/mmap_lock.o` had five functions with a canary gcc does
/// not give them.
#[test]
fn a_local_the_optimizer_turns_into_a_value_takes_its_canary_with_it() {
    let source = "\
void use(void *);
static inline void put(int *p) { *p += 1; }
int gone(int x) { int n = x; put(&n); return n; }
int kept(int x) { int n = x; use(&n); return n; }
int buf(void) { char b[16]; use(b); return 0; }
";
    let text = asm("gone", &["-O2", "-fstack-protector-strong"], source);
    assert_eq!(protected(&text), ["kept", "buf"], "{text}");
    let text = asm("gone-o0", &["-O0", "-fstack-protector-strong"], source);
    assert_eq!(protected(&text), ["gone", "kept", "buf"], "{text}");
}

/// A function that asked for its canary, or that protects everything, keeps it even when every
/// local it had has become a value.
#[test]
fn a_canary_that_was_asked_for_does_not_go_with_the_locals() {
    let source = "\
static inline void put(int *p) { *p += 1; }
__attribute__((stack_protect)) int asked(int x) { int n = x; put(&n); return n; }
int gone(int x) { int n = x; put(&n); return n; }
";
    let text = asm("asked", &["-O2", "-fstack-protector-strong"], source);
    assert_eq!(protected(&text), ["asked"], "{text}");
    let text = asm("asked-all", &["-O2", "-fstack-protector-all"], source);
    assert_eq!(protected(&text), ["asked", "gone"], "{text}");
}

/// The body of one function in an AArch64 listing, from its label to its `.size`.
fn arm64(what: &str, flags: &[&str], name: &str) -> Vec<String> {
    let out = run(what, "aarch64-unknown-linux-gnu", flags, FOUR);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .take_while(|line| !line.starts_with("\t.size"))
        .filter(|line| line.starts_with('\t') && !line.starts_with("\t."))
        .map(|line| line.trim().to_string())
        .collect()
}

/// AArch64 reads `__stack_chk_guard`, a plain global, through the global offset table by default
/// and from its own page under `-fno-pic`, which is what gcc writes for both.
#[test]
fn arm64_reads_the_global_guard_the_way_its_code_reaches_any_global() {
    let reads = |flags: &[&str]| {
        let body = arm64("arm64", flags, "buf");
        let calls = body.iter().filter(|line| *line == "bl __stack_chk_fail").count();
        assert_eq!(calls, 1, "{flags:?}: {body:?}");
        body.iter().filter(|line| line.contains("__stack_chk_guard")).cloned().collect::<Vec<_>>()
    };
    let got = reads(&["-fstack-protector"]);
    assert_eq!(got.len(), 4, "{got:?}");
    assert!(got[0].starts_with("adrp x") && got[0].ends_with(", :got:__stack_chk_guard"));
    assert!(got[1].contains(", :got_lo12:__stack_chk_guard]"), "{got:?}");
    let page = reads(&["-fstack-protector", "-fno-pic"]);
    assert_eq!(page.len(), 4, "{page:?}");
    assert!(page[0].starts_with("adrp x") && page[0].ends_with(", __stack_chk_guard"));
    assert!(page[1].ends_with(", :lo12:__stack_chk_guard"), "{page:?}");
}

/// An arm64 kernel keeps the canary in each task and passes its distance from the task, which
/// `sp_el0` holds while the kernel runs, so the word is a read of the register and one load.
#[test]
fn arm64_reads_the_kernel_canary_past_sp_el0() {
    let flags = [
        "-fstack-protector",
        "-fno-PIE",
        "-mstack-protector-guard=sysreg",
        "-mstack-protector-guard-reg=sp_el0",
        "-mstack-protector-guard-offset=1912",
    ];
    let body = arm64("sysreg", &flags, "buf");
    let mrs: Vec<usize> = (0..body.len()).filter(|&i| body[i].starts_with("mrs x")).collect();
    assert_eq!(mrs.len(), 2, "{body:?}");
    for at in mrs {
        let reg = &body[at]["mrs ".len()..body[at].find(',').expect("two operands")];
        assert!(body[at].ends_with(", sp_el0"), "{body:?}");
        let load = format!("[{reg}, #1912]");
        assert!(body[at..].iter().any(|line| line.ends_with(&load)), "{body:?}");
    }
    assert!(!body.iter().any(|line| line.contains("__stack_chk_guard")), "{body:?}");
}
