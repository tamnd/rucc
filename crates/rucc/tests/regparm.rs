//! `-mregparm=` and `__attribute__((regparm(n)))`, which put the first words of a 32 bit x86
//! function's arguments in `%eax`, `%edx` and `%ecx`. The kernel builds every 32 bit unit with
//! `-mregparm=3` and marks what assembly calls `asmlinkage`, which is `regparm(0)`.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
struct big { int a[4]; };
int third(int a, int b, int c) { return c; }
int fourth(int a, int b, int c, int d) { return d; }
int after(int a, int b, long long c, int d) { return d; }
__attribute__((regparm(0))) int linkage(int a, int b) { return b; }
int listed(int n, ...) { return n; }
struct big made(int x) { struct big b = {{x, x, x, x}}; return b; }
struct one { int a; };
struct one word(int x) { struct one r = {x}; return r; }
int second(int n, ...)
{
    __builtin_va_list ap;
    __builtin_va_start(ap, n);
    __builtin_va_arg(ap, int);
    int b = __builtin_va_arg(ap, int);
    __builtin_va_end(ap);
    return b;
}
";

/// The listing of each function, by name.
fn bodies(flags: &[&str]) -> Vec<(String, String)> {
    // The tests in this file run on threads of one process, so the process id alone would give
    // two of them the same directory and one could remove it while the other is still writing.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-regparm-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-O2", "-fno-asynchronous-unwind-tables", "-S"])
        .args(flags)
        .args(["-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let listing = String::from_utf8(out.stdout).expect("a listing is text");
    let mut bodies: Vec<(String, String)> = Vec::new();
    for line in listing.lines() {
        if let Some(name) = line.strip_suffix(':').filter(|name| !name.starts_with('.')) {
            bodies.push((name.to_owned(), String::new()));
        } else if let Some((_, body)) = bodies.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    bodies
}

fn body<'a>(bodies: &'a [(String, String)], name: &str) -> &'a str {
    bodies.iter().find(|(named, _)| named == name).map(|(_, body)| body.as_str()).expect(name)
}

/// Three words in registers, the rest on the stack, and a `long long` that does not fit in what
/// is left goes to the stack and takes the rest of the registers with it, as in gcc.
#[test]
fn three_registers_carry_the_first_three_words() {
    let bodies = bodies(&["-mregparm=3"]);
    assert!(body(&bodies, "third").contains("movl\t%ecx, %eax"), "{bodies:?}");
    assert!(body(&bodies, "fourth").contains("movl\t4(%esp), %eax"), "{bodies:?}");
    assert!(body(&bodies, "after").contains("movl\t12(%esp), %eax"), "{bodies:?}");
}

/// `regparm(0)` on one function, which is what `asmlinkage` is, and a variadic function, which
/// gcc never passes in registers, both take everything from the stack.
#[test]
fn asmlinkage_and_variadic_functions_take_the_stack() {
    let bodies = bodies(&["-mregparm=3"]);
    assert!(body(&bodies, "linkage").contains("movl\t8(%esp), %eax"), "{bodies:?}");
    assert!(body(&bodies, "listed").contains("movl\t4(%esp), %eax"), "{bodies:?}");
}

/// The address a structure is returned through is the first argument, so it is in `%eax`, and
/// with it in a register there is nothing on the stack for the callee to take off.
#[test]
fn the_return_address_is_the_first_register() {
    let bodies = bodies(&["-mregparm=3"]);
    let made = body(&bodies, "made");
    assert!(made.contains("movl\t%edx, (%eax)"), "{made}");
    assert!(!made.contains("ret\t$4"), "{made}");
}

/// Without the flag the unit is plain cdecl, and the attribute still asks for registers.
#[test]
fn without_the_flag_only_the_attribute_counts() {
    let bodies = bodies(&[]);
    assert!(body(&bodies, "third").contains("movl\t12(%esp), %eax"), "{bodies:?}");
    assert!(body(&bodies, "made").contains("ret\t$4"), "{bodies:?}");
}

/// What gcc refuses, refused in its words.
#[test]
fn a_count_above_three_or_a_machine_without_the_flag_is_refused() {
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args(args)
            .args(["-x", "c", "-S", "-o", "-", "-"])
            .stdin(std::process::Stdio::null())
            .output()
            .expect("the compiler is built before its own tests run");
        (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let (ok, said) = run(&["--target=i686-unknown-linux-gnu", "-mregparm=4"]);
    assert!(!ok && said.contains("-mregparm=4 is not between 0 and 3"), "{said}");
    let (ok, said) = run(&["--target=aarch64-unknown-linux-gnu", "-mregparm=3"]);
    assert!(!ok && said.contains("is for x86"), "{said}");
    let (ok, said) = run(&["--target=x86_64-unknown-linux-gnu", "-mregparm=3"]);
    assert!(ok && said.contains("ignored in 64 bit mode"), "{said}");
}

/// A variadic function is `regparm(0)` whatever the unit's count is, and its `va_list` is the
/// plain i386 one: the address of the word past `n`, which is where the caller left the rest.
#[test]
fn a_variadic_function_reads_its_list_off_the_stack_under_regparm() {
    let bodies = bodies(&["-mregparm=3"]);
    assert!(body(&bodies, "second").contains("leal\t12(%esp), %eax"), "{bodies:?}");
}

/// `-freg-struct-return`, which the kernel passes beside `-mregparm=3`, brings a one word structure
/// back in `eax` where the psABI writes it through the hidden pointer. A `pte_t` is one of those.
#[test]
fn a_one_word_structure_comes_back_in_eax_under_reg_struct_return() {
    let listed = bodies(&["-mregparm=3", "-freg-struct-return"]);
    let word = body(&listed, "word");
    assert!(!word.contains("(%eax)") && !word.contains("ret\t$"), "{word}");
    let big = body(&listed, "made");
    assert!(big.contains("(%eax)"), "sixteen bytes still come back in memory: {big}");
    let plain = bodies(&["-mregparm=3"]);
    assert!(body(&plain, "word").contains("(%eax)"), "{plain:?}");
}
