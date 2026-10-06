//! Which calls the inliner's second pass copies into the caller at `-O2`, which is gcc's
//! `inline_small_functions` and the hints of section 33.5 of `spec/optimizer/33-inlining.md`.
//!
//! A body over `max-inline-insns-auto` is still copied when the copy knows something the body out
//! of line does not: how many times a loop runs, or which function a call through a parameter
//! reaches. On the small shapes gcc copies each of the calls these tests copy and leaves each of
//! the calls they leave. The large ones check the bounds and the order of the heap rather than a
//! count, since gcc's own count there turns on passes this one does not have, which is
//! tamnd/rucc#2897.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so the listing is the same on
/// every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-hints-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source with those flags, and what it said.
fn compiled(what: &str, flags: &[&str], source: &str) -> (String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(flags)
        .args(["-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (String::from_utf8(out.stdout).expect("what the compiler writes is text"), said)
}

/// The assembly alone.
fn asm(what: &str, flags: &[&str], source: &str) -> String {
    compiled(what, flags, source).0
}

/// The lines of one function in the listing, from its label to the next one.
fn body<'a>(listing: &'a str, name: &str) -> Vec<&'a str> {
    let label = format!("{name}:");
    let mut lines = listing.lines().skip_while(|line| *line != label);
    let _ = lines.next();
    lines
        .take_while(|line| {
            !(line.ends_with(':') && !line.starts_with(['.', '\t', ' ']) && !line.is_empty())
        })
        .collect()
}

/// How many times that function calls or jumps to `name`, a call in tail position being a jump.
fn reaches(listing: &str, function: &str, name: &str) -> usize {
    body(listing, function)
        .iter()
        .filter(|line| {
            let line = line.trim();
            line == format!("call\t{name}") || line == format!("jmp\t{name}")
        })
        .count()
}

/// The shape from tamnd/rucc#2897. `mix` grows a caller by more than the limit, `use1` passes the
/// count of its loop, and `use2` does not.
const LOOPS: &str = "static int mix(const int *a, int n, int k)\n\
    {\n\
      int s = 0;\n\
      for (int i = 0; i < n; i++) {\n\
        int v = a[i] * k;\n\
        s += (v ^ (v >> 3)) + (v & 7) * (a[i] | 1);\n\
        if (s > 1000) s -= a[i] >> 2;\n\
        if (s < -1000) s += a[i] << 1;\n\
        s ^= (s >> 7) + i;\n\
      }\n\
      return s;\n\
    }\n\
    int use1(const int *a) { return mix(a, 4, 3); }\n\
    int use2(const int *a, int n) { return mix(a, n, 5) + mix(a + 1, n, 6); }\n";

/// `apply` calls through its first parameter. `use4` passes the address of a function and `use5`
/// passes a pointer it was given.
const INDIRECT: &str = "static int apply(int (*f)(int), const int *a, int n)\n\
    {\n\
      int s = 0;\n\
      for (int i = 0; i < n; i++) {\n\
        s += f(a[i]) * (i + 1);\n\
        if (s & 1) s ^= a[i];\n\
        s += a[i] >> 2;\n\
        if (s > 99) s -= a[i] * 3;\n\
        s ^= i << 2;\n\
      }\n\
      return s;\n\
    }\n\
    static int sq(int x) { return x * x; }\n\
    int use4(const int *a, int n) { return apply(sq, a, n); }\n\
    int use5(int (*g)(int), const int *a, int n) { return apply(g, a, n) + apply(g, a, n + 1); }\n";

#[test]
fn a_call_that_makes_a_loop_count_known_is_copied() {
    let listing = asm("loops", &["-O2"], LOOPS);
    assert_eq!(reaches(&listing, "use1", "mix"), 0, "{listing}");
    assert_eq!(reaches(&listing, "use2", "mix"), 2, "{listing}");
}

#[test]
fn a_call_that_makes_a_call_through_a_pointer_direct_is_copied() {
    let listing = asm("indirect", &["-O2"], INDIRECT);
    assert_eq!(reaches(&listing, "use4", "apply"), 0, "{listing}");
    // The call through the pointer is a call to `sq` in the copy, and that is copied as well.
    assert_eq!(reaches(&listing, "use4", "sq"), 0, "{listing}");
    assert_eq!(reaches(&listing, "use5", "apply"), 2, "{listing}");
}

#[test]
fn the_second_pass_comes_with_inline_small_functions() {
    for flags in [&["-O1"][..], &["-O2", "-fno-inline-small-functions"], &["-Os"]] {
        let listing = asm("off", flags, LOOPS);
        assert_eq!(reaches(&listing, "use1", "mix"), 1, "{flags:?}\n{listing}");
    }
    let listing = asm("o3", &["-O3"], LOOPS);
    assert_eq!(reaches(&listing, "use1", "mix"), 0, "{listing}");
}

#[test]
fn the_second_pass_says_what_it_did() {
    let (_, said) = compiled("said", &["-O2", "-fopt-info-all"], LOOPS);
    assert!(said.contains("use1: optimized: call inlined by the second pass"), "{said}");
    assert!(said.contains("use2: missed: inline call not inlined: callee too large (2)"), "{said}");
}

/// A thousand callers that would each take a copy of a body with a loop they know the count of.
/// The unit may grow by forty percent of itself and no more, so the copies stop there and the rest
/// of the calls say why.
#[test]
fn the_unit_stops_growing_at_its_bound() {
    let mut source = String::from(
        "static int mix(const int *a, int n, int k)\n\
         {\n\
           int s = 0;\n\
           for (int i = 0; i < n; i++) {\n\
             int v = a[i] * k;\n\
             s += (v ^ (v >> 3)) + (v & 7) * (a[i] | 1);\n\
             if (s > 1000) s -= a[i] >> 2;\n\
             s ^= (s >> 7) + i;\n\
           }\n\
           return s;\n\
         }\n",
    );
    for at in 0..1000 {
        source.push_str(&format!(
            "int use{at}(const int *a) {{ return mix(a, {}, {at}); }}\n",
            at % 13 + 2
        ));
    }
    let (listing, said) = compiled("unit", &["-O2", "-fopt-info-all"], &source);
    let copied = (0..1000).filter(|at| reaches(&listing, &format!("use{at}"), "mix") == 0).count();
    assert!(copied > 0, "{said}");
    assert!(copied < 1000, "every call was copied");
    assert!(said.contains("inline call not inlined: unit growth limit reached"), "{said}");
}

/// Every call passes the same count, which gcc's `ipa-cp` has put in the body before its inliner
/// asks, so no call knows more than the body does and none is copied.
#[test]
fn a_count_every_call_passes_gives_no_hint() {
    let callers: String = (0..3)
        .map(|at| format!("int use{at}(const int *a) {{ return mix(a, 4, {}); }}\n", at * 2 + 3))
        .collect();
    let source = mix_and(&callers);
    let listing = asm("settled", &["-O2"], &source);
    for at in 0..3 {
        assert_eq!(reaches(&listing, &format!("use{at}"), "mix"), 1, "{listing}");
    }
    let listing = asm("unsettled", &["-O2", "-fno-ipa-cp"], &source);
    for at in 0..3 {
        assert_eq!(reaches(&listing, &format!("use{at}"), "mix"), 0, "{listing}");
    }
}

/// A chain of three `static` functions, each called once and each too large for the limit, with
/// the first called in a loop. The second pass copies the first into `main` and the third into
/// the second, which leaves `main` calling the second once, and the called once rule after it
/// still has to see that once the first is gone. The corpus case `called-once.chain.8` is this.
#[test]
fn a_function_the_heap_leaves_called_once_is_still_inlined() {
    let step = |name: &str, next: &str, at: u32| {
        let mut body = format!("static unsigned {name}(unsigned v) {{\n");
        for k in 0..8 {
            body.push_str(&format!(
                "  v = v * {}u + {}u;\n  v ^= v >> 13;\n",
                2_654_435_761u32.wrapping_mul(at * 8 + k + 1),
                at * 1000 + k
            ));
        }
        if next.is_empty() {
            body.push_str("  return v;\n}\n");
        } else {
            body.push_str(&format!("  return {next}(v);\n}}\n"));
        }
        body
    };
    let source = format!(
        "{}{}{}unsigned run(const unsigned *a)\n{{\n  unsigned t = 0;\n  \
         for (int i = 0; i < 4096; i++) t += first(a[i]);\n  return t;\n}}\n",
        step("third", "", 3),
        step("second", "third", 2),
        step("first", "second", 1)
    );
    let listing = asm("chain", &["-O2"], &source);
    for callee in ["first", "second", "third"] {
        assert_eq!(reaches(&listing, "run", callee), 0, "{callee}\n{listing}");
    }
}

/// The body of [`LOOPS`] without its callers, for the tests that write their own.
fn mix_and(callers: &str) -> String {
    let body = LOOPS.split("int use1").next().expect("the body comes first");
    format!("{body}{callers}")
}

/// Half the callers run the call in a loop, which the guess at its frequency makes more urgent
/// than the same call run once. The unit has room for fewer copies than there are loops, so the
/// heap spends all of it on those and none on the rest. Each caller passes on a `k` it was given,
/// since a constant one can fold the body small enough for the first pass to take, and each pair
/// passes its own count, since one count at every call is a constant in the body before the hints
/// are asked.
#[test]
fn the_heap_takes_a_call_in_a_loop_before_one_run_once() {
    let mut callers = String::new();
    for at in 0..500 {
        let n = at % 13 + 2;
        callers
            .push_str(&format!("int once{at}(const int *a, int k) {{ return mix(a, {n}, k); }}\n"));
        callers.push_str(&format!(
            "int loop{at}(const int *a, int m, int k) {{ int s = 0; \
             for (int j = 0; j < m; j++) s += mix(a + j, {n}, k); return s; }}\n"
        ));
    }
    let (listing, said) = compiled("order", &["-O2", "-fopt-info-all"], &mix_and(&callers));
    let copied = |prefix: &str| {
        (0..500).filter(|at| reaches(&listing, &format!("{prefix}{at}"), "mix") == 0).count()
    };
    assert!(copied("loop") > 0, "{said}");
    assert!(copied("loop") < 500, "every call in a loop was copied");
    assert_eq!(copied("once"), 0, "a call run once went before a call in a loop");
}

/// One caller with four hundred calls it would take a copy of each. It may grow to twice what it
/// was once it is past `large-function-insns`, and the copies stop there. The counts differ from
/// call to call, since with the same count at every call gcc's `ipa-cp` makes one copy of `mix`
/// for that count and the count tells the copies nothing more.
#[test]
fn a_large_caller_stops_growing_at_its_bound() {
    let calls: String =
        (0..400).map(|at| format!("  s += mix(a, {}, {at});\n", at % 13 + 2)).collect();
    let source =
        mix_and(&format!("int big(const int *a)\n{{\n  int s = 0;\n{calls}  return s;\n}}\n"));
    let (listing, said) = compiled("function", &["-O2", "-fopt-info-all"], &source);
    let left = reaches(&listing, "big", "mix");
    assert!(left > 0, "every call was copied");
    assert!(left < 400, "{said}");
    assert!(said.contains("inline call not inlined: function growth limit reached"), "{said}");
}

/// A body with a loop over chunks that `main` calls three times with a count it knows each time.
const SLOW: &str = "static unsigned long long slow(const unsigned int *base, int chunks)\n\
    {\n\
      unsigned long long acc = 0;\n\
      for (int i = 0; i < chunks; i++) {\n\
        const unsigned int *d = base + 4 * i;\n\
        unsigned long long w = d[0] | ((unsigned long long)d[1] << 32);\n\
        unsigned int v = d[2] ^ d[3];\n\
        unsigned long long bits = 0, more = 0;\n\
        while (w != 0) { w = w & (w - 1); bits = bits + 1; }\n\
        while (v != 0) { v = v & (v - 1); more = more + 1; }\n\
        acc = acc * 31 + bits * 100 + more;\n\
      }\n\
      return acc;\n\
    }\n\
    unsigned int data[256];\n\
    int main(void) { return (int)(slow(data, 1) + slow(data, 5) + slow(data, 16)); }\n";

/// With only `main` calling it, and outside any loop, `slow` runs once each time the program does,
/// so gcc holds no call to it to be hot and leaves all three calls, though each knows the count.
/// Called from a loop somewhere else as well, it runs more often than that, and gcc copies it into
/// `main` at each of the three.
#[test]
fn a_function_that_runs_once_is_not_copied_where_it_grows_the_program() {
    let (listing, said) = compiled("once", &["-O2", "-fopt-info-all"], SLOW);
    assert_eq!(reaches(&listing, "main", "slow"), 3, "{said}\n{listing}");
    assert!(
        said.contains(
            "main: missed: inline call not inlined: call is unlikely and code size would grow"
        ),
        "{said}"
    );
    let looped = format!(
        "{SLOW}unsigned long long many(int n) {{ unsigned long long s = 0; \
         for (int j = 0; j < n; j++) s += slow(data + j, 2); return s; }}\n"
    );
    let listing = asm("looped", &["-O2"], &looped);
    assert_eq!(reaches(&listing, "main", "slow"), 0, "{listing}");
}

/// `pick` is small enough for the first pass to copy into `main` at both calls, which leaves its
/// own body calling `slow` with nothing calling it. gcc has removed that body by the time it asks
/// how often `slow` runs, so `slow` still runs once and `main` keeps all three calls.
#[test]
fn a_caller_the_first_pass_copied_everywhere_does_not_make_a_function_run_more_than_once() {
    let source = SLOW.replace(
        "int main(void) { return (int)(slow(data, 1) + slow(data, 5) + slow(data, 16)); }",
        "static unsigned long long pick(int chunks) { return slow(data, chunks); }\n\
         int main(void) { return (int)(pick(1) + pick(5) + slow(data, 16)); }",
    );
    assert!(source.contains("pick(1)"));
    let listing = asm("picked", &["-O2"], &source);
    assert_eq!(reaches(&listing, "main", "slow"), 3, "{listing}");
}
