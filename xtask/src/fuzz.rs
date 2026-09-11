//! Randomized elimination fuzzing.
//!
//! Design: `spec/safe-memory/14-verification.md` section 14.4.
//!
//! The differential accounting next door tests the paths somebody wrote down. This tests paths
//! nobody wrote. It makes C programs at random, puts exactly one memory error in each of them at a
//! point it chose and therefore knows, compiles each program twice, runs both, and holds each run
//! to the error it put there.
//!
//! # Why the programs are generated here rather than by Csmith
//!
//! Section 14.4 names Csmith and YARPGen and then says the thing that makes them awkward for this:
//! their programs are free of undefined behaviour by construction, which is exactly the wrong
//! property for a memory safety monitor, because there is nothing in them to find. Everything they
//! are worth here comes after the injection step, and the injection step needs to know what it is
//! editing. A generator that emits a hundred lines of integer arithmetic and one array is a
//! generator whose output has to be parsed back before anything can be injected into it.
//!
//! So this makes its own programs, out of a grammar small enough that the ground truth is a field
//! on a struct rather than something recovered from the text. What it gives up is the corners of
//! the language, which is the half of section 14.4 Csmith is actually for, and that half is
//! tamnd/rucc#964. What it keeps is the half that matters for elimination: an object, a loop that
//! fills it, some reads whose shape the removal rules have opinions about, and a free.
//!
//! # The two builds
//!
//! `-O0` with every removal pass turned off, and `-O2` as people ship it. Both have to report, and
//! both have to name the same judgement.
//!
//! That is a wider question than the accounting's, which builds at `-O2` twice and changes nothing
//! but the elimination. Crossing the optimization level as well means a difference here can also be
//! a check that was never inserted, or one an earlier pass moved somewhere it does not fire, and
//! those are findings this project would want. It also means the two builds differ in more than one
//! thing at once, so a failure here says less about what is wrong than a failure there does. That
//! is the right way round: this one runs on programs nobody has looked at, and its job is to notice.
//!
//! # What it has found so far
//!
//! Nothing, over three thousand programs at the seeds above one hundred thousand, which took five
//! and a half minutes. That is worth writing down rather than leaving as a green tick, because a
//! fuzzer that has never failed is as likely to be broken as it is to be finding nothing, and the
//! way to tell the difference is to check it against an oracle that is not this project. So the
//! generator was run past AddressSanitizer before any of this went in: sixty four programs, built
//! with the system compiler, and the two agreed on every one of them. Every program with an error
//! injected was caught, and every program with none ran clean.
//!
//! # What a failure leaves behind
//!
//! The program, under `target/fuzz-src`, and the seed that made it. The seed is the whole
//! reproduction: the generator is a pure function of it, so `cargo xtask fuzz --seed n --count 1`
//! makes the same program on any machine. Section 14.4 asks for a reducer as well, on the grounds
//! that it is the valuable part, and that is true of a reducer for Csmith output. These programs are
//! forty lines with one error in them and the message says where it is.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::runner::{Runner, TRIPLE};
use crate::safety::{BANNER, LEVEL, NO_ELIMINATION, OPTIMIZED, Ran, SCRIPT, TIER, read};
use crate::{Error, Result, indent, root, staticlib};

/// How many programs a run makes when nobody says.
///
/// Small, because this is in `ci` and `ci` is what people are asked to run before pushing. It is
/// not the number that finds things. The number that finds things is whatever somebody leaves
/// running with `--count`, and the point of the default is that a regression obvious enough to
/// show up in two dozen programs never reaches main.
const COUNT: u32 = 24;

/// The element types an object can be made of.
///
/// Widths one, two, four and eight, and one unsigned type, because the scale on a subscript is what
/// `discharge` reasons about when it decides a step keeps a pointer inside its object, and a
/// generator that only ever made `int` arrays would be asking about one scale.
#[derive(Debug)]
struct Elem {
    /// How it is spelled in C.
    c: &'static str,
    /// How it is spelled in an identifier.
    slug: &'static str,
}

const ELEMS: &[Elem] = &[
    Elem { c: "char", slug: "char" },
    Elem { c: "short", slug: "short" },
    Elem { c: "int", slug: "int" },
    Elem { c: "long", slug: "long" },
    Elem { c: "unsigned int", slug: "uint" },
];

/// The fewest elements an object has, and the most.
const SMALLEST: u32 = 4;
const LARGEST: u32 = 16;

/// The most objects one program allocates, and the most reads one object gets.
const OBJECTS: u32 = 3;
const READS: u32 = 2;

/// The directory the generated sources are written to.
///
/// Outside either build, because a failure message names it and a path that one of the two builds
/// owns would read as though the program belonged to that build.
const SOURCES: &str = "fuzz-src";

/// What the generator put wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    /// Nothing. The program is legal C and the monitor has to stay quiet.
    None,
    /// A read one or two elements past the end of an object.
    Overrun,
    /// A read two elements before the start of one.
    ///
    /// Two rather than one because document 03 row S5 permits the address one element before the
    /// first, deliberately, and a generator that injected the permitted case would be asserting
    /// that a decision the specification made is a bug.
    Under,
    /// A read through a pointer whose object was freed first.
    AfterFree,
    /// A second free of an object already freed.
    DoubleFree,
}

impl Fault {
    /// The judgements a report about this may name.
    ///
    /// A set rather than one value for the two spatial faults, because an out of range address can
    /// be caught where it is computed or where it is read through, and which of those happens
    /// depends on the shape of the access rather than on the error. Both are correct reports about
    /// the injected bug. The temporal faults have one answer each.
    fn judgements(self) -> &'static [u8] {
        match self {
            Self::None => &[],
            Self::Overrun | Self::Under => &[1, 2],
            Self::AfterFree => &[1],
            Self::DoubleFree => &[6],
        }
    }

    /// Every fault, in the order the generator draws from.
    const ALL: &'static [Self] =
        &[Self::None, Self::Overrun, Self::Under, Self::AfterFree, Self::DoubleFree];
}

/// How one object is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A counted loop over the whole object.
    Loop,
    /// One subscript at a fixed index.
    Index,
    /// A cursor advanced one element at a time.
    Walk,
    /// A call to a function that takes the pointer and a length.
    Helper,
}

/// One read of one object.
#[derive(Clone, Copy, Debug)]
struct Read {
    /// Which object.
    object: usize,
    /// How.
    kind: Kind,
    /// Which element, for [`Kind::Index`].
    at: u32,
}

/// One object the program allocates, fills, reads and frees.
#[derive(Clone, Copy, Debug)]
struct Object {
    /// What it holds.
    elem: &'static Elem,
    /// How many of them.
    count: u32,
}

/// One generated program and the error in it.
#[derive(Debug)]
struct Program {
    /// The file name without its extension.
    name: String,
    /// What made it, and what remakes it.
    seed: u64,
    /// What it allocates.
    objects: Vec<Object>,
    /// What it does with that.
    reads: Vec<Read>,
    /// What is wrong with it.
    fault: Fault,
    /// Which read the fault is on, or which object for the two temporal ones.
    site: usize,
}

/// Makes programs, breaks each one in a way it wrote down, and holds both builds to that.
///
/// # Errors
///
/// [`Error::Failed`] with one line per program that did not do what was injected into it, and
/// [`Error::Io`] when the run could not happen at all.
pub(crate) fn fuzz(args: &[String]) -> Result<()> {
    let (count, seed) = options(args)?;
    let programs: Vec<Program> = (0..count).map(|n| generate(seed + u64::from(n))).collect();
    let runner = Runner::find("the elimination fuzzer")?;
    println!(
        "fuzz: {count} programs from seed {seed}, {TIER}, {LEVEL} with the elimination off and \
         {OPTIMIZED} with it on, {runner}"
    );

    let sources = lay_out(&programs)?;
    let off = build(&programs, &sources, LEVEL, NO_ELIMINATION, "fuzz-base")?;
    let on = build(&programs, &sources, OPTIMIZED, &[], "fuzz-opt")?;
    let ran_off = read(&runner.run(&off, "the fuzzer")?);
    let ran_on = read(&runner.run(&on, "the fuzzer")?);

    let mut problems = Vec::new();
    let mut injected = 0;
    let mut clean = 0;
    for program in &programs {
        if program.fault == Fault::None {
            clean += 1;
        } else {
            injected += 1;
        }
        let (Some(a), Some(b)) = (ran_off.get(&program.name), ran_on.get(&program.name)) else {
            problems.push(format!("{}: did not run in both builds", program.name));
            continue;
        };
        problems.extend(program.judge(&sources, a, b));
    }

    println!(
        "fuzz: {injected} with an error injected, {clean} with none, {} that did not say what was \
         put in them",
        problems.len()
    );
    if problems.is_empty() {
        return Ok(());
    }
    Err(Error::Failed { task: "fuzz", problems })
}

/// Reads `--count` and `--seed` off the command line.
fn options(args: &[String]) -> Result<(u32, u64)> {
    let mut count = COUNT;
    let mut seed = 0u64;
    let mut at = 0;
    while at < args.len() {
        let name = args[at].as_str();
        let value = args.get(at + 1);
        match name {
            "--count" | "--seed" => {
                let Some(value) = value.and_then(|text| text.parse::<u64>().ok()) else {
                    return Err(usage(&format!("{name} wants a number")));
                };
                if name == "--count" {
                    count = u32::try_from(value).map_err(|_| usage("--count wants fewer"))?;
                } else {
                    seed = value;
                }
                at += 2;
            }
            other => return Err(usage(&format!("`{other}` is not an option this task has"))),
        }
    }
    if count == 0 {
        return Err(usage("--count 0 would generate nothing"));
    }
    Ok((count, seed))
}

fn usage(problem: &str) -> Error {
    Error::Failed { task: "fuzz", problems: vec![problem.to_owned()] }
}

/// A seeded stream of numbers.
///
/// splitmix64, because the generator has to be a pure function of the seed for a seed to be a
/// reproduction, and pulling in a crate to get that would put a dependency in `xtask` for twenty
/// lines of arithmetic.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number from zero up to but not including `n`.
    fn below(&mut self, n: u32) -> u32 {
        u32::try_from(self.next() % u64::from(n)).unwrap_or(0)
    }

    /// A number from `lo` to `hi`, both ends included.
    fn between(&mut self, lo: u32, hi: u32) -> u32 {
        lo + self.below(hi - lo + 1)
    }

    fn pick<'a, T>(&mut self, from: &'a [T]) -> &'a T {
        let n = u32::try_from(from.len()).unwrap_or(1);
        &from[self.below(n) as usize]
    }
}

/// The program one seed makes.
fn generate(seed: u64) -> Program {
    let mut rng = Rng(seed);
    let objects: Vec<Object> = (0..rng.between(1, OBJECTS))
        .map(|_| Object { elem: rng.pick(ELEMS), count: rng.between(SMALLEST, LARGEST) })
        .collect();

    let kinds = [Kind::Loop, Kind::Index, Kind::Walk, Kind::Helper];
    let mut reads = Vec::new();
    for (object, held) in objects.iter().enumerate() {
        for _ in 0..rng.between(1, READS) {
            let kind = *rng.pick(&kinds);
            reads.push(Read { object, kind, at: rng.below(held.count) });
        }
    }

    // The fault is drawn after the shape rather than before it, so that the shape is the same
    // distribution whatever gets injected into it and two seeds that differ only in what went wrong
    // are not also different programs.
    let fault = *rng.pick(Fault::ALL);
    let site = match fault {
        // An index is the only read shape with a before the start to reach, since a loop and a walk
        // both start at the object and go up. Adding one when there is none keeps the draw above
        // free of the fault.
        Fault::Under => {
            let wanted: Vec<usize> = reads
                .iter()
                .enumerate()
                .filter(|(_, r)| r.kind == Kind::Index)
                .map(|(i, _)| i)
                .collect();
            if wanted.is_empty() {
                reads.push(Read { object: 0, kind: Kind::Index, at: 0 });
                reads.len() - 1
            } else {
                *rng.pick(&wanted)
            }
        }
        Fault::Overrun => rng.below(u32::try_from(reads.len()).unwrap_or(1)) as usize,
        Fault::AfterFree | Fault::DoubleFree => {
            rng.below(u32::try_from(objects.len()).unwrap_or(1)) as usize
        }
        Fault::None => 0,
    };

    Program { name: format!("fz-{seed:05}"), seed, objects, reads, fault, site }
}

impl Program {
    /// The C this is.
    fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("void *malloc(unsigned long size);\nvoid free(void *p);\n\n");
        // Somewhere for the reads to land that no pass may throw away. A value main computes and
        // does not return is a value the optimizer is entitled to delete, and deleting it deletes
        // the accesses and the checks on them, which would leave a fuzzer that generates programs
        // with nothing in them and passes.
        out.push_str("unsigned long taken;\n");

        for elem in self.helpers() {
            let _ = write!(
                out,
                "\nstatic unsigned long sum_{}({} *a, unsigned long n) {{\n    unsigned long total \
                 = 0;\n    unsigned long i;\n    for (i = 0; i < n; i++) {{\n        total += \
                 (unsigned long)a[i];\n    }}\n    return total;\n}}\n",
                elem.slug, elem.c
            );
        }

        out.push_str("\nint main(void) {\n    unsigned long sink = 0;\n    unsigned long i = 0;\n");
        for (n, object) in self.objects.iter().enumerate() {
            let _ = writeln!(
                out,
                "    {} *p{n} = malloc({} * sizeof({}));",
                object.elem.c, object.count, object.elem.c
            );
        }
        for (n, read) in self.reads.iter().enumerate() {
            if read.kind == Kind::Walk {
                let _ = writeln!(out, "    {} *q{n};", self.objects[read.object].elem.c);
            }
        }

        // Every object filled before anything reads any of them, so that nothing here is ever a
        // read of storage that was never written. That is a real bug class and the monitor catches
        // it, and it is not the one being injected, so a program that had one by accident would
        // report for the wrong reason and count as a pass.
        out.push('\n');
        for (n, object) in self.objects.iter().enumerate() {
            let _ = writeln!(
                out,
                "    for (i = 0; i < {}; i++) {{ p{n}[i] = ({})i; }}",
                object.count, object.elem.c
            );
        }

        out.push('\n');
        let mut freed = vec![false; self.objects.len()];
        for (n, read) in self.reads.iter().enumerate() {
            if self.fault == Fault::AfterFree && read.object == self.site && !freed[read.object] {
                freed[read.object] = true;
                let _ = writeln!(out, "    free(p{});", read.object);
            }
            self.emit(&mut out, n, read);
        }

        out.push('\n');
        for (n, freed) in freed.iter().enumerate() {
            if !freed {
                let _ = writeln!(out, "    free(p{n});");
            }
            if self.fault == Fault::DoubleFree && n == self.site {
                let _ = writeln!(out, "    free(p{n});");
            }
        }

        out.push_str("\n    taken = sink;\n    return 0;\n}\n");
        out
    }

    /// One read, with the fault in it if this is where the fault is.
    fn emit(&self, out: &mut String, n: usize, read: &Read) {
        let object = &self.objects[read.object];
        let over = self.fault == Fault::Overrun && self.site == n;
        let under = self.fault == Fault::Under && self.site == n;
        let p = read.object;
        match read.kind {
            Kind::Loop => {
                let limit = if over { object.count + 1 } else { object.count };
                let _ = writeln!(
                    out,
                    "    for (i = 0; i < {limit}; i++) {{ sink += (unsigned long)p{p}[i]; }}"
                );
            }
            Kind::Index => {
                let at = match (over, under) {
                    (true, _) => format!("{}", object.count + 1),
                    // Two elements below the first, which is one past the window row S5 allows.
                    (_, true) => "-2".to_owned(),
                    _ => format!("{}", read.at),
                };
                let _ = writeln!(out, "    sink += (unsigned long)p{p}[{at}];");
            }
            Kind::Walk => {
                // Two past rather than one, because the cursor is advanced after the read and a
                // walk of count plus one would only ever compute the address one past the end,
                // which is an address C permits and this monitor permits.
                let limit = if over { object.count + 2 } else { object.count };
                let _ = writeln!(out, "    q{n} = p{p};");
                let _ = writeln!(
                    out,
                    "    for (i = 0; i < {limit}; i++) {{ sink += (unsigned long)*q{n}; q{n}++; }}"
                );
            }
            Kind::Helper => {
                let length = if over { object.count + 1 } else { object.count };
                let _ = writeln!(out, "    sink += sum_{}(p{p}, {length});", object.elem.slug);
            }
        }
    }

    /// The element types this program needs a summing function for.
    fn helpers(&self) -> Vec<&'static Elem> {
        let mut wanted: Vec<&'static Elem> = Vec::new();
        for read in &self.reads {
            let elem = self.objects[read.object].elem;
            if read.kind == Kind::Helper && !wanted.iter().any(|had| had.slug == elem.slug) {
                wanted.push(elem);
            }
        }
        wanted
    }

    /// Whether the two builds did what was injected.
    fn judge(&self, sources: &Path, off: &Ran, on: &Ran) -> Vec<String> {
        let mut problems = Vec::new();
        for (level, ran) in [(LEVEL, off), (OPTIMIZED, on)] {
            if let Err(problem) = self.held(ran) {
                problems.push(format!(
                    "{}: at {level}, {problem}\n{}",
                    self.name,
                    self.provenance(sources)
                ));
            }
        }
        if !problems.is_empty() || self.fault == Fault::None {
            return problems;
        }
        let (was, now) = (judgement(off), judgement(on));
        if was != now {
            problems.push(format!(
                "{}: refused for J{} at {LEVEL} with the elimination off and J{} at {OPTIMIZED} \
                 with it on, so the two builds disagree about what the error is\n{}",
                self.name,
                was.unwrap_or(0),
                now.unwrap_or(0),
                self.provenance(sources)
            ));
        }
        problems
    }

    /// Whether one build did.
    fn held(&self, ran: &Ran) -> std::result::Result<(), String> {
        let spoke = ran.output.contains(BANNER);
        if self.fault == Fault::None {
            if spoke {
                return Err(format!(
                    "the program has no error in it and the monitor reported anyway\n{}",
                    indent(ran.output.trim_end())
                ));
            }
            if ran.status != Some(0) {
                return Err(format!(
                    "the program has no error in it and exited {:?}\n{}",
                    ran.status,
                    indent(ran.output.trim_end())
                ));
            }
            return Ok(());
        }
        if !spoke {
            return Err(format!(
                "{} and the monitor said nothing\n{}",
                self.describe(),
                indent(ran.output.trim_end())
            ));
        }
        let Some(judgement) = judgement(ran) else {
            return Err(format!(
                "the monitor reported and named no judgement\n{}",
                indent(ran.output.trim_end())
            ));
        };
        if self.fault.judgements().contains(&judgement) {
            return Ok(());
        }
        Err(format!(
            "{} and the monitor refused it for J{judgement}\n{}",
            self.describe(),
            indent(ran.output.trim_end())
        ))
    }

    /// What was injected, in a sentence.
    fn describe(&self) -> String {
        match self.fault {
            Fault::None => "nothing was injected".to_owned(),
            Fault::Overrun => {
                format!("p{} is read past its end", self.reads[self.site].object)
            }
            Fault::Under => {
                format!("p{} is read before its start", self.reads[self.site].object)
            }
            Fault::AfterFree => format!("p{} is read after it was freed", self.site),
            Fault::DoubleFree => format!("p{} is freed twice", self.site),
        }
    }

    /// Where to look and how to get it back.
    fn provenance(&self, sources: &Path) -> String {
        indent(&format!(
            "{}\n{}\ncargo xtask fuzz --seed {} --count 1",
            self.describe(),
            sources.join(format!("{}.c", self.name)).display(),
            self.seed
        ))
    }
}

/// Which judgement a run named, if it named one.
fn judgement(ran: &Ran) -> Option<u8> {
    ran.output
        .split_once("judgement J")
        .and_then(|(_, rest)| rest.split_once(','))
        .and_then(|(number, _)| number.parse().ok())
}

/// Writes every generated program out, and says where they went.
fn lay_out(programs: &[Program]) -> Result<PathBuf> {
    let dir = root().join("target").join(SOURCES);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", dir.display())))?;
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", dir.display())))?;
    for program in programs {
        let path = dir.join(format!("{}.c", program.name));
        std::fs::write(&path, program.render())
            .map_err(|e| Error::Io(format!("could not write {}: {e}", path.display())))?;
    }
    Ok(dir)
}

/// Compiles every program one way and lays out the directory the runner is pointed at.
fn build(
    programs: &[Program],
    sources: &Path,
    level: &str,
    without: &[&str],
    dir: &str,
) -> Result<PathBuf> {
    let work = root().join("target").join(dir);
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;

    let status = Command::new("cargo")
        .args(["build", "-q", "--release", "-p", "rucc"])
        .current_dir(root())
        .status()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if !status.success() {
        return Err(Error::Io("the compiler did not build".to_owned()));
    }
    let rucc = root().join("target").join("release").join("rucc");
    let archive = staticlib("rucc-safe-rt", TRIPLE)?;
    std::fs::copy(&archive, work.join("safe-rt.a"))
        .map_err(|e| Error::Io(format!("could not copy {}: {e}", archive.display())))?;

    let mut problems = Vec::new();
    for program in programs {
        let out = Command::new(&rucc)
            .args(["-S", &format!("--target={TRIPLE}"), TIER, level])
            .args(without)
            .arg("-o")
            .arg(work.join(format!("{}.s", program.name)))
            .arg(sources.join(format!("{}.c", program.name)))
            .current_dir(root())
            .output()
            .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
        if !out.status.success() {
            problems.push(format!(
                "{}: did not compile at {level}\n{}\n{}",
                program.name,
                indent(String::from_utf8_lossy(&out.stderr).trim_end()),
                program.provenance(sources)
            ));
        }
    }
    if !problems.is_empty() {
        return Err(Error::Failed { task: "fuzz", problems });
    }

    std::fs::write(work.join("run.sh"), SCRIPT)
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    Ok(work)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_seed_makes_one_program_however_often_it_is_asked() {
        // The whole reproduction story. A seed that made a different program on the machine
        // somebody is debugging on would make the message this prints a lie.
        assert_eq!(generate(7).render(), generate(7).render());
        assert_ne!(generate(7).render(), generate(8).render());
    }

    /// The first seed whose program has this fault in it.
    fn with(fault: Fault) -> Program {
        (0..2000)
            .map(generate)
            .find(|program| program.fault == fault)
            .expect("two thousand seeds cover five faults")
    }

    #[test]
    fn a_clean_program_reads_every_object_inside_itself_and_frees_each_of_them_once() {
        let program = with(Fault::None);
        let text = program.render();
        assert!(!text.contains("[-2]"), "{text}");
        for (n, object) in program.objects.iter().enumerate() {
            assert!(!text.contains(&format!("p{n}[{}]", object.count + 1)), "{text}");
            assert_eq!(text.matches(&format!("free(p{n});")).count(), 1, "{text}");
        }
    }

    #[test]
    fn an_overrun_reaches_one_element_past_the_object_it_is_on() {
        let program = with(Fault::Overrun);
        let text = program.render();
        let object = &program.objects[program.reads[program.site].object];
        let past = object.count + 1;
        assert!(
            text.contains(&format!("i < {past};"))
                || text.contains(&format!("[{past}]"))
                || text.contains(&format!(", {past});"))
                || text.contains(&format!("i < {};", object.count + 2)),
            "{text}"
        );
    }

    #[test]
    fn an_underrun_reaches_past_the_element_before_the_first_that_row_s5_allows() {
        // One before is permitted on purpose, so injecting it would be injecting correct code and
        // then failing the run for not reporting it.
        let text = with(Fault::Under).render();
        assert!(text.contains("[-2]"), "{text}");
        assert!(!text.contains("[-1]"), "{text}");
    }

    #[test]
    fn a_use_after_free_frees_the_object_before_something_reads_it() {
        let program = with(Fault::AfterFree);
        let text = program.render();
        let free = format!("free(p{});", program.site);
        assert_eq!(text.matches(&free).count(), 1, "{text}");
        let at = text.find(&free).expect("the free is there");
        let read = text[at..].find(&format!("p{}", program.site));
        assert!(read.is_some(), "nothing reads the object after the free\n{text}");
    }

    #[test]
    fn a_double_free_frees_the_same_object_twice_and_the_others_once() {
        let program = with(Fault::DoubleFree);
        let text = program.render();
        for n in 0..program.objects.len() {
            let want = if n == program.site { 2 } else { 1 };
            assert_eq!(text.matches(&format!("free(p{n});")).count(), want, "{text}");
        }
    }

    #[test]
    fn a_program_that_calls_a_helper_defines_the_one_it_calls() {
        let program = (0..2000)
            .map(generate)
            .find(|program| program.reads.iter().any(|read| read.kind == Kind::Helper))
            .expect("two thousand seeds reach every read shape");
        let text = program.render();
        for elem in program.helpers() {
            assert!(text.contains(&format!("static unsigned long sum_{}(", elem.slug)), "{text}");
        }
    }

    fn ran(output: &str) -> Ran {
        Ran { output: output.to_owned(), status: Some(0) }
    }

    fn reported(judgement: u8) -> Ran {
        Ran {
            output: format!("{BANNER}\n  judgement J{judgement}, something\n"),
            status: Some(134),
        }
    }

    #[test]
    fn an_injected_error_the_monitor_missed_is_a_problem() {
        let program = with(Fault::Overrun);
        let problems = program.judge(Path::new("/tmp"), &ran(""), &reported(1));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("said nothing"), "{problems:?}");
    }

    #[test]
    fn an_injected_error_both_builds_caught_the_same_way_is_not() {
        let program = with(Fault::DoubleFree);
        assert!(program.judge(Path::new("/tmp"), &reported(6), &reported(6)).is_empty());
    }

    #[test]
    fn a_report_naming_a_judgement_the_injected_error_is_not_about_is_a_problem() {
        let program = with(Fault::DoubleFree);
        let problems = program.judge(Path::new("/tmp"), &reported(1), &reported(1));
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].contains("refused it for J1"), "{problems:?}");
    }

    #[test]
    fn two_builds_that_caught_it_differently_are_a_problem_even_though_both_caught_it() {
        let program = with(Fault::Overrun);
        let problems = program.judge(Path::new("/tmp"), &reported(2), &reported(1));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("disagree about what the error is"), "{problems:?}");
    }

    #[test]
    fn a_clean_program_that_reported_is_a_problem_and_one_that_did_not_is_not() {
        let program = with(Fault::None);
        assert!(program.judge(Path::new("/tmp"), &ran(""), &ran("")).is_empty());
        let problems = program.judge(Path::new("/tmp"), &ran(""), &reported(1));
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("no error in it and the monitor reported"), "{problems:?}");
    }

    #[test]
    fn the_count_and_the_seed_are_read_off_the_command_line() {
        let args = ["--count", "3", "--seed", "11"].map(str::to_owned);
        assert_eq!(options(&args).expect("both are numbers"), (3, 11));
        assert_eq!(options(&[]).expect("neither is required"), (COUNT, 0));
        assert!(options(&["--count".to_owned(), "0".to_owned()]).is_err());
        assert!(options(&["--what".to_owned()]).is_err());
    }
}
