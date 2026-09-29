//! The address of a local and the address of a name are written where they are read.
//!
//! Either one is a single instruction that reads nothing the program can change, a `lea` off the
//! stack pointer or off the instruction pointer, so writing it again costs what reloading it from
//! a stack slot would. Kept in a register from the entry block instead, the two functions below
//! each wanted more registers than a call leaves alone, pushed all of those and spilled the rest
//! of the addresses to slots of their own. tamnd/rucc#2200 has the numbers: gcc 14 at `-O2` takes
//! 80 bytes for each, and rucc took 144 and 128.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The two reduced programs from the issue. The first reads the address of one of ten locals in
/// each arm of a switch, and the second passes twelve string literals to one call.
const SOURCE: &str = r#"
extern int pick(void);
extern void get(int *);
extern void appendf(void *buf, const char *fmt, ...);

int frame_addrs(void)
{
	int a, b, c, d, e, f, g, h, i, j;
	int s = 0;

	while (pick()) {
		switch (pick()) {
		case 0: get(&a); s += a; break;
		case 1: get(&b); s += b; break;
		case 2: get(&c); s += c; break;
		case 3: get(&d); s += d; break;
		case 4: get(&e); s += e; break;
		case 5: get(&f); s += f; break;
		case 6: get(&g); s += g; break;
		case 7: get(&h); s += h; break;
		case 8: get(&i); s += i; break;
		default: get(&j); s += j; break;
		}
	}
	return s;
}

void help_text(void *buf, int which)
{
	if (which)
		appendf(buf, "%s %s %s %s %s %s %s %s %s %s %s %s",
				"ALTER", "TABLE", "ADD", "COLUMN", "DROP", "CONSTRAINT",
				"RENAME", "TO", "SET", "SCHEMA", "OWNER", "TABLESPACE");
	else
		appendf(buf, "%s %s %s %s %s %s %s %s %s %s %s %s",
				"CREATE", "INDEX", "ON", "USING", "WITH", "WHERE",
				"INCLUDE", "NULLS", "FIRST", "LAST", "ASC", "DESC");
}
"#;

/// A directory of its own for one compile, so that two tests running at once do not share files.
fn scratch(what: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-rebuilt-{}-{n}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// What one compile wrote: the assembly and the `.su` beside it.
struct Compiled {
    asm: String,
    usage: String,
}

fn compile(target: &str, level: &str) -> Compiled {
    let dir = scratch(&format!("{target}{level}"));
    std::fs::write(dir.join("one.c"), SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .arg(format!("--target={target}"))
        .args([level, "-fstack-usage", "-S", "one.c", "-o", "one.s"])
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let read = |name: &str| {
        std::fs::read_to_string(dir.join(name)).unwrap_or_else(|_| panic!("{name} was written"))
    };
    let compiled = Compiled { asm: read("one.s"), usage: read("one.su") };
    let _ = std::fs::remove_dir_all(&dir);
    compiled
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

/// One function's instructions, from its label to the directive that closes it.
fn body<'a>(asm: &'a str, function: &str) -> Vec<&'a str> {
    asm.lines()
        .skip_while(|line| line.trim() != format!("{function}:"))
        .skip(1)
        .map(str::trim)
        .take_while(|line| !line.starts_with(".size") && !line.starts_with(".cfi_endproc"))
        .filter(|line| !line.is_empty() && !line.starts_with('.'))
        .collect()
}

/// The registers an instruction writes an address into, when it is one that computes an address
/// of a local or of a name.
fn address_into(line: &str) -> Option<&str> {
    // x86-64: `leaq 12(%rsp), %rax` and `leaq .LC3(%rip), %rax`.
    if let Some(rest) = line.strip_prefix("leaq ") {
        return rest.rsplit(", ").next();
    }
    // AArch64: `add x0, sp, #12` and the `add` after an `adrp`.
    let rest = line.strip_prefix("add ")?;
    let mut parts = rest.split(", ");
    let into = parts.next()?;
    let from = parts.next()?;
    let what = parts.next()?;
    (from == "sp" || what.starts_with(":lo12:")).then_some(into)
}

/// Whether an operand is a place in this function's frame.
fn in_frame(operand: &str) -> bool {
    ["(%rsp)", "(%rbp)"].iter().any(|base| operand.contains(base))
        || ["[sp", "[x29"].iter().any(|base| operand.starts_with(base))
}

/// A move between a register and the frame, as the register and the slot. x86-64 writes the
/// register first for a store and last for a load, and AArch64 writes it first for both.
fn slot_of<'a>(line: &'a str, op: &str) -> Option<(&'a str, &'a str)> {
    let (left, right) = line.strip_prefix(op)?.split_once(", ")?;
    if in_frame(left) {
        return Some((right, left));
    }
    in_frame(right).then_some((left, right))
}

/// The register an instruction writes, as far as telling an address apart from what replaced it
/// needs: the last operand on x86-64 and the first on AArch64, for anything that writes one.
fn written(line: &str) -> Option<&str> {
    let (op, operands) = line.split_once(' ')?;
    let mut operands = operands.split(", ");
    if line.contains('%') {
        let last = operands.next_back()?;
        let reads = ["cmp", "test", "push", "call", "j"].iter().any(|kind| op.starts_with(kind));
        return (last.starts_with('%') && !reads).then_some(last);
    }
    let reads = ["str", "stp", "cmp", "cmn", "tst", "cb", "tb", "b", "ret"];
    if reads.iter().any(|kind| op.starts_with(kind)) {
        return None;
    }
    operands.next()
}

/// The frame slots an address was stored into and later read back out of, which is what a spilled
/// address looks like. A store of an address into the argument area of a call is not one, since
/// nothing in this function reads that back.
fn spilled_addresses<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let mut holding: Vec<&str> = Vec::new();
    let mut stored: Vec<&str> = Vec::new();
    let mut spilled = Vec::new();
    for line in lines {
        if let Some((reg, slot)) = slot_of(line, "movq ").or_else(|| slot_of(line, "str ")) {
            if !line.ends_with(reg) && holding.contains(&reg) {
                stored.push(slot);
            }
        }
        if let Some((_, slot)) = slot_of(line, "movq ").or_else(|| slot_of(line, "ldr ")) {
            // A store's slot is last on x86-64, so a `movq` with the slot first is a load.
            let load = line.starts_with("ldr ") || !line.ends_with(slot);
            if load && stored.contains(&slot) && !spilled.contains(&slot) {
                spilled.push(slot);
            }
        }
        if line.starts_with("call") || line.starts_with("bl ") {
            holding.clear();
        }
        match address_into(line) {
            Some(into) => holding.push(into),
            None => {
                if let Some(into) = written(line) {
                    holding.retain(|&held| held != into);
                }
            }
        }
    }
    spilled
}

/// The frames the issue measured, as gcc 14 lays them out at `-O2` on x86-64. rucc's own were
/// 144 and 128.
#[test]
fn a_local_s_address_and_a_string_s_address_are_not_kept_in_the_frame() {
    for level in ["-O2", "-O0"] {
        let compiled = compile("x86_64-unknown-linux-gnu", level);
        let (asm, usage) = (&compiled.asm, &compiled.usage);
        for (function, gcc) in [("frame_addrs", 80), ("help_text", 80)] {
            let bytes = frame(usage, function);
            assert!(bytes <= gcc, "{level} {function}: {bytes} bytes, gcc takes {gcc}\n{asm}");
        }
    }
}

/// The twelve strings go out one at a time, each a `lea` and the store that passes it, so the
/// call needs none of the registers a call leaves alone and pushes nothing.
#[test]
fn a_call_with_twelve_strings_pushes_no_register() {
    let compiled = compile("x86_64-unknown-linux-gnu", "-O2");
    let lines = body(&compiled.asm, "help_text");
    let text = lines.join("\n");
    assert!(!lines.iter().any(|line| line.starts_with("push")), "{text}");
}

/// No address is stored into a slot and read back, on either machine, at either level.
#[test]
fn no_address_is_spilled() {
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        for level in ["-O2", "-O0"] {
            let compiled = compile(target, level);
            for function in ["frame_addrs", "help_text"] {
                let lines = body(&compiled.asm, function);
                let spilled = spilled_addresses(&lines);
                assert!(
                    spilled.is_empty(),
                    "{target} {level} {function}: {spilled:?}\n{}\n{}",
                    lines.join("\n"),
                    compiled.usage
                );
            }
        }
    }
}

/// In the switch each arm works out the address of its own local, rather than the entry block
/// working all ten out ahead of the loop.
#[test]
fn a_local_s_address_is_written_in_the_arm_that_reads_it() {
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let compiled = compile(target, "-O2");
        let lines = body(&compiled.asm, "frame_addrs");
        let text = lines.join("\n");
        let first_call = lines
            .iter()
            .position(|line| line.starts_with("call") || line.starts_with("bl "))
            .unwrap_or_else(|| panic!("{target}: frame_addrs calls pick\n{text}"));
        let early = lines[..first_call].iter().filter(|line| address_into(line).is_some()).count();
        assert_eq!(early, 0, "{target}: addresses worked out ahead of the loop\n{text}");
    }
}
