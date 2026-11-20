//! A local whose address nothing uses any more takes no room in the frame.
//!
//! The kernel's `iwl_rfi_send_config_cmd` in 7.3 returns early when `iwl_rfi_supported` says
//! false, which it always does. Once that call is folded every use of the 700 byte `cmd` goes, and
//! the local was left in the frame with nothing using it. The frame was 792 bytes, where gcc's is 8.

use std::path::PathBuf;
use std::process::Command;

/// Two shapes: the address of the local kept in a second local that is turned into values, and
/// the address of the local used only past a test that always fails.
const SOURCE: &str = r"
struct Cmd { char table[700]; int oem; };
extern int send(void *);

int supported(void *m) { return 0; }

int kept(void *m, const char *table)
{
	struct Cmd cmd;
	void *p = &cmd;
	if (!supported(m))
		return -95;
	__builtin_memcpy(cmd.table, table, sizeof cmd.table);
	return send(&p);
}

__attribute__((noinline)) int never(void *m) { return 0; }

int guarded(void *m, const char *table)
{
	struct Cmd cmd;
	if (!never(m))
		return -95;
	__builtin_memcpy(cmd.table, table, sizeof cmd.table);
	return send(&cmd);
}
";

/// The `.su` one compile wrote.
fn usage(target: &str) -> String {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("rucc-dead-local-{}-{target}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .arg(format!("--target={target}"))
        .args(["-O2", "-fstack-usage", "-S", "one.c", "-o", "one.s"])
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let usage = std::fs::read_to_string(dir.join("one.su")).expect("one.su was written");
    let _ = std::fs::remove_dir_all(&dir);
    usage
}

/// The bytes the `.su` says a function takes.
fn frame(usage: &str, function: &str) -> u32 {
    usage
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let place = fields.next()?;
            place.ends_with(&format!(":{function}")).then(|| fields.next()?.parse().ok())?
        })
        .unwrap_or_else(|| panic!("no line for {function} in\n{usage}"))
}

#[test]
fn a_local_nothing_addresses_takes_no_room_in_the_frame() {
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let usage = usage(target);
        for function in ["kept", "guarded"] {
            let bytes = frame(&usage, function);
            assert!(bytes <= 32, "{target}: {bytes} bytes for {function}");
        }
    }
}
