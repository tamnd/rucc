//! A thread-local in an i386 shared library is reached by a call to `___tls_get_addr`, as gcc
//! writes it. rucc used the initial exec model there, which reads a slot of the global offset
//! table, and a library built that way asks for static TLS, which `dlopen` may refuse. The call
//! goes through the procedure linkage table, so the table's address must be in `%ebx` for it.

use std::process::Command;

const SOURCE: &str = "\
extern __thread int counter;
static __thread int hidden;
int bump(void) { return ++counter + ++hidden; }
int *where(void) { return &counter; }
";

fn compiled(what: &str, args: &[&str]) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("rucc-i386-dyntls-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("--target=i686-unknown-linux-gnu")
        .args(args)
        .arg(&path)
        .current_dir(&dir)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.success(), said, String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn a_library_reaches_a_thread_local_through_a_call_with_the_table_in_ebx() {
    for level in ["-O0", "-O2"] {
        let (ok, text, err) = compiled(&level[1..], &[level, "-fPIC", "-S", "-o", "-"]);
        assert!(ok, "{level}: {err}");
        assert!(text.contains("leal\tcounter@tlsgd(,%ebx,1), %eax"), "{level}:\n{text}");
        assert!(text.contains("@tlsldm(%ebx), %eax"), "{level}:\n{text}");
        assert!(text.contains("hidden@dtpoff(%eax), %eax"), "{level}:\n{text}");
        assert!(text.contains("call\t___tls_get_addr@PLT"), "{level}:\n{text}");
        assert!(text.contains("call\t__x86.get_pc_thunk.bx"), "{level}:\n{text}");
        assert!(!text.contains("@gotntpoff") && !text.contains("%gs:"), "{level}:\n{text}");
    }
}

#[test]
fn the_call_assembles_and_an_executable_keeps_the_initial_exec_model() {
    let (ok, _, err) = compiled("object", &["-O2", "-fPIC", "-c", "-o", "one.o"]);
    assert!(ok, "{err}");
    let (ok, text, err) = compiled("exec", &["-O2", "-fno-pic", "-S", "-o", "-"]);
    assert!(ok, "{err}");
    assert!(!text.contains("___tls_get_addr"), "{text}");
}
