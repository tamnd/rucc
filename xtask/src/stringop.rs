//! Times a block copy and a block fill written out as moves against the same written as a call.
//!
//! Design: `spec/optimizer/40-cost-models.md` sections 40.4, 40.7 and 40.14, and experiment 61 of
//! `spec/optimizer/42-measurement.md` (tamnd/rucc#2971). Section 40.4 holds rucc's block copy
//! thresholds to the standard gcc sets with `contrib/bench-stringop`: the numbers are measured, and
//! what measured them ships next to them. This is that harness for `BLOCK_COPY_MOVES_FOR_SPEED`
//! and `BLOCK_CLEAR_MOVES_FOR_SPEED`.
//!
//! # What is timed
//!
//! A copy and a fill of every size from 1 to 256 bytes, at every alignment up to 16 that the size
//! allows, each in a function of its own: a structure assignment for the copy and a `memset` of
//! zero for the fill, the two shapes the thresholds decide for. The same source is compiled twice
//! by the compiler this tree builds, once with `--param` setting both thresholds high enough that
//! every one of them is moves and once with both at nought so that every one is a call to the C
//! library, so the two sides differ in the one thing the threshold chooses and in nothing else.
//!
//! A driver runs every function on both sides once to check what it wrote, then times them both,
//! alternately, for a number of rounds, and keeps the fastest timing of each. The fastest is the
//! timing least disturbed by whatever else the machine is doing, and alternating means a machine
//! that gets busier halfway through slows both sides of a ratio rather than one. The time is the
//! thread's own processor time rather than the wall clock, so a stretch the thread spent waiting
//! for a processor is not in it.
//!
//! # What it says
//!
//! For each of the two, every stretch of sixteen bytes with the mean time of both sides over every
//! size and alignment in it, and the size at which the moves stop being faster: the end of the last
//! stretch before the first one where the call is as fast or faster. That size in eight byte moves
//! is what the two thresholds are set from, and section 40.7 has what it said last.
//!
//! A size whose call side was moves after all, because something before the lowering had already
//! turned the copy into a load and a store, is not a row: there is nothing on that side to time.
//!
//! `--count` answers the other half of the issue, which is where the sizes real code copies are.
//! It compiles the files it is given with both thresholds at nought, so every copy and fill of a
//! known size is a call, and counts the sizes the calls pass.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::bench::{commit, host};
use crate::cost::compiler;
use crate::runner::{Runner, TRIPLE};
use crate::{Error, Result, target_dir};

/// What this task is called, for the messages.
const TASK: &str = "stringop";

/// The largest size timed, which is the 32 moves the lowering used to stop at.
const LARGEST: u32 = 256;

/// The alignments a size is timed at, where the size is a multiple of them.
const ALIGNMENTS: [u32; 5] = [1, 2, 4, 8, 16];

/// The width of one move, which is what a size is counted in when it becomes a threshold.
const WORD: u32 = 8;

/// How many sizes one row of the report covers.
const STRETCH: u32 = 16;

/// How many rounds are timed by default.
const ROUNDS: usize = 15;

/// The two operations, as the functions are named.
const OPS: [&str; 2] = ["copy", "clear"];

/// The two sides, as the functions are named, with what both thresholds are set to on each.
/// Sixty four moves covers the largest size at any alignment the plan uses on x86-64.
const SIDES: [(&str, u32); 2] = [("moves", 64), ("call", 0)];

/// The two rows of `--print-params` both sides set.
const PARAMS: [&str; 2] = ["block-copy-moves-for-speed", "block-clear-moves-for-speed"];

/// Runs the harness and prints the report.
///
/// # Errors
///
/// [`Error::Io`] when the options are wrong or a side will not build, and [`Error::Failed`] when a
/// function wrote the wrong bytes or the moves side called the library after all.
pub(crate) fn stringop(args: &[String]) -> Result<()> {
    if args.first().is_some_and(|a| a == "--count") {
        return count(&args[1..]);
    }
    let mut rounds = ROUNDS;
    let mut csv = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--rounds" => {
                let value = rest.next().ok_or_else(|| bad("--rounds needs a count"))?;
                rounds = value.parse().map_err(|_| bad("--rounds needs a count"))?;
                if rounds == 0 {
                    return Err(bad("--rounds needs at least 1"));
                }
            }
            "--csv" => csv = true,
            other => return Err(bad(format!("unknown option `{other}`"))),
        }
    }

    let rucc = compiler()?;
    let cases = cases();
    let (work, skipped) = build(&rucc, &cases, rounds)?;
    let runner = Runner::find("the block copy harness")?;
    let printed = runner.run(&work, "the block copy harness")?;
    let rows = read(&printed)?;
    if csv {
        println!("op,size,align,moves,call");
        for row in &rows {
            println!("{},{},{},{:.3},{:.3}", row.op, row.size, row.align, row.moves, row.call);
        }
    } else {
        report(&rows, &skipped, rounds, runner);
    }
    Ok(())
}

/// Every size and alignment timed, in the order the report reads them.
fn cases() -> Vec<(u32, u32)> {
    (1..=LARGEST)
        .flat_map(|size| {
            ALIGNMENTS.iter().filter(move |&&a| size % a == 0).map(move |&a| (size, a))
        })
        .collect()
}

/// The functions of one side.
///
/// Each ends in an empty `asm` that clobbers memory, so the call on the call side is a call and
/// not a jump the function leaves by, which is what a copy in the middle of real code pays for.
fn side(name: &str, cases: &[(u32, u32)]) -> String {
    let mut c = String::new();
    for &(size, align) in cases {
        let ty = format!("t{size}_{align}");
        let _ = writeln!(
            c,
            "typedef struct {{ unsigned char b[{size}]; }} __attribute__((aligned({align}))) {ty};"
        );
        let _ = writeln!(
            c,
            "void {name}_copy_{size}_{align}({ty} *d, const {ty} *s) {{ *d = *s; \
             __asm__ volatile(\"\" ::: \"memory\"); }}"
        );
        let _ = writeln!(
            c,
            "void {name}_clear_{size}_{align}({ty} *d, const {ty} *s) {{ (void)s; \
             __builtin_memset(d, 0, {size}); __asm__ volatile(\"\" ::: \"memory\"); }}"
        );
    }
    c
}

/// The driver, which checks every function once and then times them.
fn driver(cases: &[(u32, u32)], rounds: usize) -> String {
    let mut c = String::from("#include <stdio.h>\n#include <time.h>\n");
    c.push_str("typedef void (*fn)(void *, const void *);\n");
    for (side, _) in SIDES {
        for op in OPS {
            for &(size, align) in cases {
                let _ = writeln!(c, "void {side}_{op}_{size}_{align}(void *, const void *);");
            }
        }
    }
    c.push_str(
        "static const struct row { const char *op; int size, align; fn side[2]; } rows[] = {\n",
    );
    for op in OPS {
        for &(size, align) in cases {
            let _ = writeln!(
                c,
                "  {{\"{op}\", {size}, {align}, {{moves_{op}_{size}_{align}, call_{op}_{size}_{align}}}}},"
            );
        }
    }
    c.push_str("};\n");
    c.push_str(&DRIVER.replace("@ROUNDS@", &rounds.to_string()));
    c
}

/// The part of the driver that does not depend on the cases.
///
/// Both buffers start 64 bytes in and then the alignment further, so an address aligned to four
/// is aligned to four and to nothing wider. The check fills the destination with a byte neither
/// side writes, runs the function, and looks at the bytes it should have written and one on each
/// side of them.
const DRIVER: &str = r#"
static unsigned char from[1024] __attribute__((aligned(64)));
static unsigned char to[1024] __attribute__((aligned(64)));

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_THREAD_CPUTIME_ID, &t);
    return t.tv_sec * 1e9 + t.tv_nsec;
}

static int wrong(const struct row *r, int side) {
    unsigned char *d = to + 64 + r->align, *s = from + 64 + r->align;
    for (int i = 0; i < 1024; i++) to[i] = 0xa5;
    r->side[side](d, s);
    if (d[-1] != 0xa5 || d[r->size] != 0xa5) return 1;
    for (int i = 0; i < r->size; i++)
        if (d[i] != (r->op[1] == 'o' ? s[i] : 0)) return 1;
    return 0;
}

static double once(fn f, int align) {
    unsigned char *d = to + 64 + align, *s = from + 64 + align;
    double start = now();
    for (int i = 0; i < 20000; i++) {
        f(d, s);
        __asm__ volatile("" ::: "memory");
    }
    return (now() - start) / 20000;
}

int main(void) {
    int count = sizeof rows / sizeof rows[0];
    for (int i = 0; i < 1024; i++) from[i] = (unsigned char)(i * 7 + 1);
    for (int r = 0; r < count; r++)
        for (int side = 0; side < 2; side++)
            if (wrong(&rows[r], side))
                printf("wrong %s %d %d %s\n", rows[r].op, rows[r].size, rows[r].align,
                       side ? "call" : "moves");
    for (int r = 0; r < count; r++) {
        double best[2] = {1e30, 1e30};
        for (int round = 0; round < @ROUNDS@; round++)
            for (int side = 0; side < 2; side++) {
                double t = once(rows[r].side[side], rows[r].align);
                if (t < best[side]) best[side] = t;
            }
        printf("row %s %d %d %.3f %.3f\n", rows[r].op, rows[r].size, rows[r].align, best[0],
               best[1]);
    }
    return 0;
}
"#;

/// What the runner runs, which links the two sides with the driver and runs it.
const SCRIPT: &str = "\
#!/bin/sh
out=/tmp/stringop
mkdir -p \"$out\"
gcc -O2 -o \"$out/stringop\" driver.c moves.o call.o || exit 1
\"$out/stringop\"
";

/// Writes both sides and the driver, compiles the sides, and lays out the directory the runner is
/// pointed at. Says which sizes the call side did not call for.
fn build(rucc: &Path, cases: &[(u32, u32)], rounds: usize) -> Result<(PathBuf, Vec<String>)> {
    let work = target_dir().join(TASK);
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;

    let mut skipped = Vec::new();
    for (name, moves) in SIDES {
        let source = work.join(format!("{name}.c"));
        write(&source, &side(name, cases))?;
        let mut flags = vec![format!("--target={TRIPLE}"), "-O2".to_owned()];
        for param in PARAMS {
            flags.push("--param".to_owned());
            flags.push(format!("{param}={moves}"));
        }
        let listing = work.join(format!("{name}.s"));
        compile(rucc, &flags, "-S", &listing, &source)?;
        compile(rucc, &flags, "-c", &work.join(format!("{name}.o")), &source)?;
        let text = std::fs::read_to_string(&listing)
            .map_err(|e| Error::Io(format!("could not read {}: {e}", listing.display())))?;
        let calls = calling(&text);
        for op in OPS {
            for &(size, align) in cases {
                let function = format!("{name}_{op}_{size}_{align}");
                let called = calls.contains(&function);
                if name == "moves" && called {
                    return Err(Error::Failed {
                        task: TASK,
                        problems: vec![format!(
                            "{function} calls the library with both thresholds at {moves}, so the \
                             moves side is not moves"
                        )],
                    });
                }
                if name == "call" && !called {
                    skipped.push(format!("{op} {size} {align}"));
                }
            }
        }
    }
    write(&work.join("driver.c"), &driver(cases, rounds))?;
    write(&work.join("run.sh"), SCRIPT)?;
    Ok((work, skipped))
}

/// One file written, or the error that says which.
fn write(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text)
        .map_err(|e| Error::Io(format!("could not write {}: {e}", path.display())))
}

/// Runs the compiler once.
fn compile(rucc: &Path, flags: &[String], mode: &str, out: &Path, source: &Path) -> Result<()> {
    let done = Command::new(rucc)
        .args(flags)
        .arg(mode)
        .arg("-o")
        .arg(out)
        .arg(source)
        .output()
        .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
    if !done.status.success() {
        return Err(Error::Failed {
            task: TASK,
            problems: vec![format!(
                "{} did not compile with {}\n{}",
                source.display(),
                flags.join(" "),
                crate::indent(String::from_utf8_lossy(&done.stderr).trim_end())
            )],
        });
    }
    Ok(())
}

/// The functions of a listing that call `memcpy` or `memset`.
fn calling(listing: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut current = "";
    for line in listing.lines() {
        if let Some(label) = line.strip_suffix(':').filter(|l| !l.starts_with(['.', '\t', ' '])) {
            current = label;
        } else if line.contains("memcpy") || line.contains("memset") {
            let call =
                line.trim_start().starts_with("call") || line.trim_start().starts_with("jmp");
            if call && found.last().is_none_or(|last: &String| last != current) {
                found.push(current.to_owned());
            }
        }
    }
    found
}

/// One size and alignment timed on both sides.
#[derive(Debug)]
struct Row {
    op: String,
    size: u32,
    align: u32,
    /// Nanoseconds for one, written out as moves.
    moves: f64,
    /// Nanoseconds for one, as a call.
    call: f64,
}

/// Turns what the driver printed into rows, and refuses a run where a function wrote the wrong
/// bytes.
fn read(printed: &str) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    let mut problems = Vec::new();
    for line in printed.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["wrong", op, size, align, side] => problems.push(format!(
                "the {side} side of a {op} of {size} bytes aligned to {align} wrote the wrong bytes"
            )),
            ["row", op, size, align, moves, call] => {
                let number = |w: &str| w.parse::<f64>().map_err(|_| bad(format!("`{line}`")));
                rows.push(Row {
                    op: (*op).to_owned(),
                    size: size.parse().map_err(|_| bad(format!("`{line}`")))?,
                    align: align.parse().map_err(|_| bad(format!("`{line}`")))?,
                    moves: number(moves)?,
                    call: number(call)?,
                });
            }
            _ => {}
        }
    }
    if rows.is_empty() {
        problems.push("the driver printed no rows, so nothing was timed".to_owned());
    }
    if !problems.is_empty() {
        return Err(Error::Failed { task: TASK, problems });
    }
    Ok(rows)
}

/// Prints the report.
fn report(rows: &[Row], skipped: &[String], rounds: usize, runner: Runner) {
    println!(
        "block copies and fills on {}, {} at {}, fastest of {rounds} rounds",
        host(),
        runner,
        commit()
    );
    println!("times are nanoseconds for one, the ratio is the moves over the call");
    for op in OPS {
        let mine: Vec<&Row> =
            rows.iter().filter(|r| r.op == op && !skipped.contains(&key(r))).collect();
        println!();
        println!("{op}");
        println!("  {:<9} {:>7} {:>7} {:>7}   moves faster", "bytes", "moves", "call", "ratio");
        let mut faster_to = None;
        let mut settled = false;
        for low in (1..=LARGEST).step_by(STRETCH as usize) {
            let high = low + STRETCH - 1;
            let band: Vec<&&Row> = mine.iter().filter(|r| (low..=high).contains(&r.size)).collect();
            if band.is_empty() {
                continue;
            }
            let n = band.len() as f64;
            let moves = band.iter().map(|r| r.moves).sum::<f64>() / n;
            let call = band.iter().map(|r| r.call).sum::<f64>() / n;
            let wins = band.iter().filter(|r| r.moves < r.call).count();
            println!(
                "  {:<9} {moves:>7.2} {call:>7.2} {:>6.2}x   {wins} of {}",
                format!("{low}-{high}"),
                moves / call,
                band.len()
            );
            if !settled {
                if moves < call {
                    faster_to = Some(high);
                } else {
                    settled = true;
                }
            }
        }
        match faster_to {
            Some(bytes) => println!(
                "  the moves are faster up to {bytes} bytes, which is {} moves of {WORD} bytes",
                bytes.div_ceil(WORD)
            ),
            None => println!("  the call is faster from the first size"),
        }
    }
    if !skipped.is_empty() {
        println!();
        println!(
            "{} sizes were moves on the call side too and are not in the report: {}",
            skipped.len(),
            skipped.join(", ")
        );
    }
}

/// How a row is named in the list of skipped ones.
fn key(row: &Row) -> String {
    format!("{} {} {}", row.op, row.size, row.align)
}

/// Compiles the files given at `-O2` with both thresholds at nought and counts the sizes of the
/// copies and fills of a known size, in moves of a word.
///
/// The size is read from the `mov` of a constant into the third argument register nearest before
/// the call, so a copy whose size the compiler worked out some other way is not counted. Anything
/// after `--` is passed to the compiler as it is, which is where SQLite's `-D` flags go.
fn count(args: &[String]) -> Result<()> {
    let (files, flags) = match args.iter().position(|a| a == "--") {
        Some(at) => (&args[..at], &args[at + 1..]),
        None => (args, &args[args.len()..]),
    };
    if files.is_empty() {
        return Err(bad("--count needs the C files to count in"));
    }
    let rucc = compiler()?;
    let out = target_dir().join(TASK).join("count.s");
    std::fs::create_dir_all(target_dir().join(TASK))
        .map_err(|e| Error::Io(format!("could not make the work directory: {e}")))?;
    let mut sizes: Vec<(String, u64)> = Vec::new();
    let mut failed = 0;
    for file in files {
        let mut all = vec![format!("--target={TRIPLE}"), "-O2".to_owned()];
        for param in PARAMS {
            all.push("--param".to_owned());
            all.push(format!("{param}=0"));
        }
        all.extend(flags.iter().cloned());
        if compile(&rucc, &all, "-S", &out, Path::new(file)).is_err() {
            failed += 1;
            continue;
        }
        let text = std::fs::read_to_string(&out)
            .map_err(|e| Error::Io(format!("could not read {}: {e}", out.display())))?;
        sizes.extend(sized(&text));
    }
    let bands: [(u64, u64); 7] =
        [(1, 2), (3, 4), (5, 8), (9, 12), (13, 16), (17, 32), (33, u64::MAX)];
    println!("{} files, {failed} would not compile", files.len());
    for op in ["memcpy", "memset"] {
        let mine: Vec<u64> = sizes.iter().filter(|(o, _)| o == op).map(|&(_, s)| s).collect();
        println!();
        println!("{op}: {} of a known size", mine.len());
        let mut below = 0;
        for (low, high) in bands {
            let n = mine
                .iter()
                .filter(|&&s| (low..=high).contains(&s.div_ceil(u64::from(WORD))))
                .count();
            below += n;
            let share =
                |k: usize| if mine.is_empty() { 0.0 } else { 100.0 * k as f64 / mine.len() as f64 };
            let label =
                if high == u64::MAX { format!("{low} or more") } else { format!("{low}-{high}") };
            println!(
                "  {label:<11} moves {n:>7} {:>6.1}%  {:>6.1}% up to here",
                share(n),
                share(below)
            );
        }
    }
    Ok(())
}

/// The size of every call to `memcpy` and `memset` in a listing whose size is a constant.
fn sized(listing: &str) -> Vec<(String, u64)> {
    let mut found = Vec::new();
    let mut third: Option<u64> = None;
    for line in listing.lines() {
        let line = line.trim();
        if line.ends_with(':') {
            third = None;
            continue;
        }
        if let Some(op) = ["memcpy", "memset"].into_iter().find(|op| line.contains(op)) {
            if line.starts_with("call") || line.starts_with("jmp") {
                if let Some(size) = third.take() {
                    found.push((op.to_owned(), size));
                }
                continue;
            }
        }
        if line.ends_with("%edx")
            || line.ends_with("%rdx")
            || line.ends_with("%dx")
            || line.ends_with("%dl")
        {
            third = line
                .strip_prefix("movl\t$")
                .or_else(|| line.strip_prefix("movq\t$"))
                .and_then(|rest| rest.split(',').next())
                .and_then(|n| n.parse().ok());
        } else if line.starts_with("call") {
            third = None;
        }
    }
    found
}

fn bad(message: impl Into<String>) -> Error {
    Error::Io(format!("{TASK}: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::{calling, cases, sized};

    #[test]
    fn every_size_is_timed_at_every_alignment_it_allows() {
        let all = cases();
        assert!(all.contains(&(256, 16)));
        assert!(all.contains(&(13, 1)));
        assert!(!all.contains(&(12, 8)));
        assert_eq!(all.iter().filter(|&&(size, _)| size == 24).count(), 4);
    }

    #[test]
    fn a_function_that_calls_the_library_is_found_and_one_that_does_not_is_not() {
        let listing = "moves_copy_8_8:\n\tmovq\t(%rsi), %rax\n\tmovq\t%rax, (%rdi)\n\tret\n\
                       call_copy_96_8:\n\tmovl\t$96, %edx\n\tcall\tmemcpy@PLT\n\tret\n";
        assert_eq!(calling(listing), vec!["call_copy_96_8".to_owned()]);
        assert_eq!(sized(listing), vec![("memcpy".to_owned(), 96)]);
    }

    #[test]
    fn a_size_set_before_another_call_is_not_the_size_of_the_next_one() {
        let listing = "f:\n\tmovl\t$40, %edx\n\tcall\tg\n\tcall\tmemset\n";
        assert!(sized(listing).is_empty());
    }
}
