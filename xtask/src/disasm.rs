//! The differential disassembly check.
//!
//! `spec/11-asm-objects-debug.md` section 11.1 says encoding correctness is verified by
//! disassembling what we encode with an independent decoder and holding it to what we meant. This
//! is that check. It reads the listing `cargo run -p rucc-target --example listing` prints, which
//! is every instruction the target writes as bytes on one side of a bar and as assembly on the
//! other, and asks a decoder that has never seen our table what each of the two halves says.
//!
//! What is compared is the two accounts and not the two byte strings. We choose a longer encoding
//! in a few places on purpose, and will until relaxation is written, so bytes differ where nothing
//! is wrong. A decoder reading our bytes and a decoder reading an assembler's bytes for the text
//! we printed have to name the same instruction with the same operands, and where they do not,
//! either the bytes are wrong or the text is, and both are a bug in one description.
//!
//! The decoder is `llvm-mc`, which is not a build dependency and not needed to build or test the
//! compiler. This task is the only thing that wants it.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, Result, root};

/// The machine the listing is written for, and the machine the decoder is asked about.
const TRIPLE: &str = "--triple=x86_64-unknown-linux-gnu";

/// Runs the differential disassembly check over every instruction the target writes.
pub(crate) fn disasm() -> Result<()> {
    let tool = find()?;
    let listing = listing()?;
    let mut ours = Vec::new();
    let mut text = String::from(".text\n");
    for line in listing.lines() {
        let Some((bytes, written)) = line.split_once('|') else {
            return Err(Error::Io(format!("listing line has no bar in it: {line}")));
        };
        ours.push(bytes.to_owned());
        text.push_str(written);
        text.push('\n');
    }
    println!("xtask: {} instructions, decoding both halves with {}", ours.len(), tool.display());

    let theirs = assemble(&tool, &text)?;
    if theirs.len() != ours.len() {
        return Err(Error::Io(format!(
            "the assembler made {} instructions out of {} lines",
            theirs.len(),
            ours.len()
        )));
    }
    let mine = decode(&tool, &ours)?;
    let yours = decode(&tool, &theirs)?;
    if mine.len() != ours.len() || yours.len() != ours.len() {
        return Err(Error::Io(format!(
            "the decoder read {} and {} instructions out of {}",
            mine.len(),
            yours.len(),
            ours.len()
        )));
    }

    let mut problems = Vec::new();
    for (at, ((meant, said), bytes)) in yours.iter().zip(&mine).zip(&ours).enumerate() {
        if meant != said {
            problems
                .push(format!("line {}: `{meant}` encoded to {bytes}, which is `{said}`", at + 1));
        }
    }
    if problems.is_empty() {
        println!("xtask: every instruction reads back as the one it was written from");
        return Ok(());
    }
    Err(Error::Failed { task: "disasm", problems })
}

/// The decoder, wherever it is on this machine.
///
/// It is not on the path on a mac, where homebrew keeps it out of the way of the system one, and
/// a linux distribution puts it under a directory named for its version, so both are looked in.
/// Where there are several, the last by name wins, and which one it is does not much matter: any
/// decoder that reads this machine reads it the same way, which is the point of asking one.
fn find() -> Result<PathBuf> {
    let mut versioned: Vec<PathBuf> = std::fs::read_dir("/usr/lib")
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("llvm-"))
        })
        .map(|path| path.join("bin").join("llvm-mc"))
        .collect();
    versioned.sort();
    versioned.reverse();
    let candidates = [
        PathBuf::from("llvm-mc"),
        PathBuf::from("/opt/homebrew/opt/llvm/bin/llvm-mc"),
        PathBuf::from("/usr/local/opt/llvm/bin/llvm-mc"),
    ];
    for path in candidates.into_iter().chain(versioned) {
        if Command::new(&path).arg("--version").output().is_ok_and(|out| out.status.success()) {
            return Ok(path);
        }
    }
    Err(Error::Io(
        "no llvm-mc on this machine, which is the decoder this check reads with. \
         It ships with llvm: `brew install llvm` or `apt install llvm`."
            .to_owned(),
    ))
}

/// The listing, straight out of the example that prints it.
fn listing() -> Result<String> {
    let out = Command::new("cargo")
        .args(["run", "-q", "-p", "rucc-target", "--example", "listing"])
        .current_dir(root())
        .output()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "the listing example failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What an assembler makes of the text we printed, one instruction's bytes per entry.
///
/// `--show-encoding` puts the bytes in a comment beside each instruction, which is a way of asking
/// for them that does not go through an object file and so does not need a second tool to read one.
fn assemble(tool: &Path, text: &str) -> Result<Vec<String>> {
    let source = root().join("target").join("listing.s");
    std::fs::create_dir_all(source.parent().expect("the file is under a directory"))
        .map_err(|e| Error::Io(format!("could not make a place for the listing: {e}")))?;
    std::fs::write(&source, text)
        .map_err(|e| Error::Io(format!("could not write {}: {e}", source.display())))?;
    let out = run(tool, &["--assemble", TRIPLE, "--show-encoding"], Some(&source))?;
    Ok(out
        .lines()
        .filter_map(|line| line.split_once("# encoding: [")?.1.strip_suffix(']'))
        .map(|bytes| bytes.replace("0x", "").replace(',', " "))
        .collect())
}

/// What the decoder says each of those byte strings is.
fn decode(tool: &Path, instructions: &[String]) -> Result<Vec<String>> {
    let mut hex = String::new();
    for bytes in instructions {
        for byte in bytes.split_whitespace() {
            hex.push_str("0x");
            hex.push_str(byte);
            hex.push(' ');
        }
        hex.push('\n');
    }
    let source = root().join("target").join("listing.hex");
    std::fs::write(&source, hex)
        .map_err(|e| Error::Io(format!("could not write {}: {e}", source.display())))?;
    let out = run(tool, &["--disassemble", TRIPLE], Some(&source))?;
    Ok(out.lines().filter_map(tidy).collect())
}

/// One line of the decoder's output, as an instruction, or nothing if it is not one.
///
/// The decoder writes a tab between the mnemonic and the operands and sometimes a comment saying
/// what an immediate is in hexadecimal. Neither is part of the instruction.
fn tidy(line: &str) -> Option<String> {
    let line = line.split('#').next().unwrap_or(line).trim();
    if line.is_empty() || line.starts_with('.') {
        return None;
    }
    Some(one(&line.split_whitespace().collect::<Vec<_>>().join(" ")))
}

/// Says a shift by one the long way, whichever way the decoder said it.
///
/// The machine has a shift that carries no count and means one, and an assembler picks it where we
/// write the general form with a one in it. They are the same instruction, and the check is about
/// what an instruction is rather than how few bytes it took, so both are read as the long form.
/// The decoder writes an immediate in decimal, so the one put back is written the same way.
fn one(inst: &str) -> String {
    let shifts = ["shl", "shr", "sar", "sal", "rol", "ror", "rcl", "rcr"];
    let Some((mnemonic, operand)) = inst.split_once(' ') else {
        return inst.to_owned();
    };
    let shifted = shifts.iter().any(|shift| mnemonic.starts_with(shift));
    if shifted && !operand.contains(',') {
        return format!("{mnemonic} $1, {operand}");
    }
    inst.to_owned()
}

/// Runs the decoder over a file and hands back what it printed.
fn run(tool: &Path, args: &[&str], input: Option<&Path>) -> Result<String> {
    let mut command = Command::new(tool);
    command.args(args);
    if let Some(path) = input {
        command.arg(path);
    }
    let out = command
        .output()
        .map_err(|e| Error::Io(format!("could not run {}: {e}", tool.display())))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "{} {} failed: {}",
            tool.display(),
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
