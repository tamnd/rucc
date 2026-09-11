//! Target and psABI queries, and the generator for `docs/TARGETS.md`.
//!
//! Design: `spec/cross-compile/03-target-model.md` and `spec/cross-compile/06-abis.md`.
//!
//! Everything here is a question about a target that has an answer before any code is emitted
//! for it. `spec/cross-compile/04-target-matrix.md` section 4.2 has a tier for a target the
//! compiler knows about and cannot emit for, and this is what that tier promises: a user can
//! find out what rucc thinks their machine is before finding out there is no back end for it.
//!
//! It is a build tool rather than an `xtask` task because `xtask` has no dependencies on
//! purpose and these answers come from reading `rucc-tuple` and `rucc-abi` rather than from a
//! copy of what they say. A copy of a table is a table that drifts, which is the failure
//! `spec/cross-compile/04-target-matrix.md` section 4.7 is written against.
//!
//! The argument parsing is by hand. There are ten subcommands and a handful of flags, so a
//! dependency here would be a dependency in the workspace for a `match` on a string.

mod corpus;
mod docs;
mod lines;
mod rng;
mod signatures;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;

use rucc_abi::{AbiDescription, DataLayout, FloatType, Rule, abis};
use rucc_sysroot::argv::{emulation, pe_machine};
use rucc_sysroot::link::loader;
use rucc_sysroot::{LinkLine, LinkMode, Sysroot, layout::can_be_bundled};
use rucc_tuple::{TARGETS, TargetTuple, lookup, planned_working_count};

const USAGE: &str = "\
usage: cargo run -q -p rucc-targets -- <command>

commands:
  list              every target and what is true of it
  info <tuple>      everything the compiler derives from one tuple
  abi <tuple>       the type layout and the psABI, as described
  sysroot <tuple>   where the headers are and what order the link inputs go in
  canonical <tuple> the canonical spelling
  llvm-triple <tup> the LLVM spelling, for tools that want one
  docs              write docs/TARGETS.md from the table
  docs --check      check that docs/TARGETS.md matches the table
  abi-corpus <tup>  the record layout corpus for one target, on standard output
  abi-corpus --write   write tests/abi-corpus for every target that has one
  abi-corpus --check   check that tests/abi-corpus matches the grammar
  abi-signatures --write  write tests/abi-signatures, which is one program
  abi-signatures --check  check that tests/abi-signatures matches the grammar
  link-lines <tup>  the linker command line for one target, in every mode
  link-lines --write   write tests/link-lines for every target
  link-lines --check   check that tests/link-lines matches what would be written
  help
";

/// The stand-in for the cache directory in `sysroot` output.
///
/// A literal rather than the real cache, so that two people running the command on two machines
/// see the same text and can compare it. The real path is whatever the driver was given, and the
/// only part of it this crate decides is everything after it.
const CACHE: &str = "<cache>";

/// The workspace root, which is two directories above this crate.
///
/// From the manifest directory rather than from the current directory, so that the answer does
/// not depend on where the tool was run from. `spec/cross-compile/02-the-goal.md` claim 5 is
/// about output that does not depend on the machine, and a path that depends on the shell's
/// working directory is the same bug one level up.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the manifest directory is two below the workspace root")
        .to_path_buf()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();

    match borrowed.as_slice() {
        [] | ["--help"] | ["-h"] | ["help"] => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        ["list"] => {
            list();
            ExitCode::SUCCESS
        }
        ["docs"] => docs::run(&root(), false),
        ["docs", "--check"] => docs::run(&root(), true),
        ["abi-corpus", "--write"] => corpus::run(&root(), corpus::Mode::Write),
        ["abi-corpus", "--check"] => corpus::run(&root(), corpus::Mode::Check),
        ["abi-corpus", tuple] => match TargetTuple::from_str(tuple) {
            Ok(target) => corpus::one(target),
            Err(error) => {
                eprintln!("error: {error}");
                eprintln!("  in target tuple `{tuple}`");
                ExitCode::FAILURE
            }
        },
        ["abi-signatures", "--write"] => signatures::run(&root(), signatures::Mode::Write),
        ["abi-signatures", "--check"] => signatures::run(&root(), signatures::Mode::Check),
        ["link-lines", "--write"] => lines::run(&root(), lines::Mode::Write),
        ["link-lines", "--check"] => lines::run(&root(), lines::Mode::Check),
        ["link-lines", tuple] => match TargetTuple::from_str(tuple) {
            Ok(target) => lines::one(target),
            Err(error) => {
                eprintln!("error: {error}");
                eprintln!("  in target tuple `{tuple}`");
                ExitCode::FAILURE
            }
        },
        ["info", tuple] => run(tuple, info),
        ["abi", tuple] => run(tuple, abi),
        ["sysroot", tuple] => run(tuple, sysroot),
        ["canonical", tuple] => run(tuple, |t| println!("{}", t.to_canonical_string())),
        ["llvm-triple", tuple] => run(tuple, |t| println!("{}", t.to_llvm_string())),
        _ => {
            eprint!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

/// Parse a tuple and hand it to a command, or print the diagnostic and fail.
///
/// The diagnostic is the whole point of the exercise, so it goes to standard error unadorned
/// and the exit status is non zero. A tool that prints an error and exits zero is a tool that
/// breaks a build script silently.
fn run(tuple: &str, body: impl FnOnce(TargetTuple)) -> ExitCode {
    match TargetTuple::from_str(tuple) {
        Ok(target) => {
            body(target);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("  in target tuple `{tuple}`");
            ExitCode::FAILURE
        }
    }
}

/// Print the table, and the counts computed from it rather than written next to it.
fn list() {
    let width = TARGETS.iter().map(|entry| entry.tuple.len()).max().unwrap_or(0);
    println!("{:width$}  now  plan  host   notes", "target");
    for entry in TARGETS {
        println!(
            "{:width$}  {:^3}  {:^4}  {:<5}  {}",
            entry.tuple,
            entry.tier.number(),
            entry.planned.number(),
            entry.host,
            entry.note
        );
    }
    println!();
    println!(
        "{} rows, {} of which the plan compiles and links",
        TARGETS.len(),
        planned_working_count()
    );
}

/// Print everything the compiler derives from one tuple.
///
/// This is the tier 4 promise from `spec/04-target-matrix.md`: a target that emits nothing still
/// answers this correctly, so a user can find out what rucc thinks their machine is before
/// finding out there is no back end for it.
fn info(target: TargetTuple) {
    let model = target.data_model();
    let row = lookup(&target);

    println!("canonical      {}", target.to_canonical_string());
    println!("llvm           {}", target.to_llvm_string());
    println!("arch           {}", target.arch());
    if target.sub_arch() != rucc_tuple::SubArch::None {
        println!("baseline       {}", target.sub_arch().as_str());
    }
    println!("endian         {}", target.endian());
    println!("os             {}", target.os());
    if let Some(version) = target.os_version() {
        println!("os version     {version}");
    }
    println!(
        "env            {}",
        if target.env().as_str().is_empty() { "none" } else { target.env().as_str() }
    );
    if let Some(version) = target.env_version() {
        println!("env version    {version}");
    }
    if target.arch().selects_float_abi() {
        println!("abi            {}", target.resolved_abi());
    }
    println!("object format  {}", target.object_format());
    println!("data model     {model}");
    println!("int            {} bits", model.int_width());
    println!("long           {} bits", model.long_width());
    println!("long long      {} bits", model.long_long_width());
    println!("pointer        {} bits", model.pointer_width());
    println!("address        {} bits", model.address_width());
    println!("char           {}", if target.char_is_signed() { "signed" } else { "unsigned" });
    println!("underscore     {}", if target.leading_underscore() { "yes" } else { "no" });

    match row {
        Some(entry) => {
            println!("tier           {} ({})", entry.tier, entry.tier.describe());
            println!("planned tier   {} ({})", entry.planned, entry.planned.describe());
            println!("host           {}", entry.host);
            if !entry.note.is_empty() {
                println!("note           {}", entry.note);
            }
        }
        None => {
            println!("tier           not in the table");
        }
    }
}

/// Print the type layout and the psABI for one tuple.
///
/// The layout half prints for every target, because it is derived from the tuple and there is
/// always an answer. The ABI half prints only where there is a description, and says so plainly
/// where there is not, because `spec/06-abis.md` opens by naming the failure this avoids: a
/// program that works for months and then does not, because something answered a question about
/// an ABI with the almost-right answer.
fn abi(target: TargetTuple) {
    let layout = DataLayout::for_target(target);

    println!("target         {}", target.to_canonical_string());
    println!("char           {}", if layout.char_is_signed { "signed" } else { "unsigned" });
    println!("short          {} bytes", layout.short_size);
    println!("int            {} bytes", layout.int_size);
    println!("long           {} bytes", layout.long_size);
    println!(
        "long long      {} bytes, aligned to {}",
        layout.long_long_size, layout.long_long_align
    );
    println!("pointer        {} bytes, aligned to {}", layout.pointer_size, layout.pointer_align);
    println!("float          {}", float(layout.float));
    println!("double         {}", float(layout.double));
    println!("long double    {}", float(layout.long_double));
    println!(
        "bit-fields     {}",
        match layout.bitfield_order {
            rucc_abi::BitfieldOrder::LowestFirst => "first field in the low order bits",
            rucc_abi::BitfieldOrder::HighestFirst => "first field in the high order bits",
        }
    );
    match layout.max_field_align {
        Some(cap) => println!("member align   capped at {cap} bytes"),
        None => println!("member align   uncapped"),
    }
    println!("underscore     {}", if layout.leading_underscore { "yes" } else { "no" });
    println!();

    let Some(described) = abis::for_target(target) else {
        println!("abi            not described yet");
        println!();
        println!(
            "The type layout above is still correct, because it comes from the tuple. What is"
        );
        println!("missing is how a value travels in a call, and answering that with a nearby ABI");
        println!("would be worse than not answering it.");
        return;
    };
    rules(described);
}

/// Print where one target's sysroot is, what is in it, and what order the link inputs go in.
///
/// Every line comes from the tuple. Nothing here reads the filesystem, so the answer is the same
/// whether the sysroot has been built yet or not, and the same on every machine, which is the
/// property `spec/cross-compile/02-the-goal.md` claim 5 asks for and the reason the cache
/// directory prints as a placeholder rather than as wherever this run happened to look.
fn sysroot(target: TargetTuple) {
    let sysroot = Sysroot::in_cache(Path::new(CACHE), target);

    println!("target         {}", target.to_canonical_string());
    println!("cache key      {}", sysroot.cache_key());
    println!("root           {}", sysroot.root().display());
    println!("headers        {}", sysroot.arch_include().display());
    println!("               {}", sysroot.generic_include().display());
    println!("link inputs    {}", sysroot.lib().display());
    println!("manifest       {}", sysroot.manifest_path().display());
    println!("header arch    {}", sysroot.header_arch());
    println!(
        "bundled        {}",
        if can_be_bundled(target) {
            "yes"
        } else {
            "no, the platform's headers are not ours to ship"
        }
    );
    println!();

    // The file names rather than the whole command line, because the ordering is what this
    // subcommand is for and the flags that go with it are long enough to push the names off the
    // edge of a terminal. `link-lines <tuple>` prints the command itself.
    for (mode, name) in [
        (LinkMode::Static, "static"),
        (LinkMode::StaticPie, "static-pie"),
        (LinkMode::Dynamic, "dynamic"),
        (LinkMode::DynamicNoPie, "dynamic-no-pie"),
        (LinkMode::Shared, "shared"),
    ] {
        let line = LinkLine::for_target(&sysroot, mode);
        let names: Vec<String> = line
            .with_objects(&[PathBuf::from("main.o")])
            .iter()
            .map(|path| {
                path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
            })
            .collect();
        println!("{name:<15}{}", names.join(" "));
    }

    println!();
    println!("loader         {}", loader(target).unwrap_or("none on the link line"));
    let machine = emulation(target).or_else(|| pe_machine(target));
    println!("emulation      {}", machine.unwrap_or("none, the format has no names"));
}

/// One floating point type, as the two separate facts it is.
fn float(kind: FloatType) -> String {
    format!("{}, {} bytes, aligned to {}", kind.format.name(), kind.size, kind.align)
}

/// Print the description of one ABI, in the order the classifier reads it.
fn rules(described: &'static AbiDescription) {
    println!("abi            {}", described.name);
    if described.banks.shared {
        println!("registers      {} shared argument positions", described.banks.integer);
    } else {
        println!(
            "registers      {} integer, {} floating point",
            described.banks.integer, described.banks.float
        );
    }
    println!(
        "widths         {} byte integer registers, {} byte vector registers",
        described.banks.integer_width, described.banks.float_width
    );
    if let Some(format) = described.scalars.in_memory {
        println!("in memory      a scalar {} argument, spending no register", format.name());
    }
    if described.scalars.wide_integer_is_all_or_nothing {
        println!("wide integers  take every register they need or none of them");
    }
    println!(
        "return pointer {}",
        match described.return_pointer {
            rucc_abi::ReturnPointer::FirstArgument =>
                "a hidden first argument, spending a register",
            rucc_abi::ReturnPointer::Dedicated => "a register outside the argument bank",
        }
    );
    println!(
        "variadic       {}",
        match described.variadic {
            rucc_abi::Variadic::SameAsFixed => "classified the same way a fixed argument is",
            rucc_abi::Variadic::AlwaysMemory =>
                "always in the argument area, whatever registers are left",
            rucc_abi::Variadic::BothBanks => "a floating point value travels in both banks",
        }
    );
    println!(
        "stack args     {}",
        match described.stack_args {
            rucc_abi::StackArgs::RegisterSized => "a whole register's worth each",
            rucc_abi::StackArgs::Packed => "packed at their natural size",
        }
    );

    for (which, list) in [("returns", described.returns), ("arguments", described.arguments)] {
        println!();
        println!("{which}");
        for rule in list {
            println!("  {}", rule_line(rule));
        }
    }
}

/// One rule, as a sentence.
fn rule_line(rule: &Rule) -> String {
    use rucc_abi::{Short, Test, Travel};

    let when = match rule.when {
        Test::Anything => "anything else".to_string(),
        Test::Empty => "an aggregate of no size".to_string(),
        Test::SizeOneOf(sizes) => {
            let list: Vec<String> = sizes.iter().map(u64::to_string).collect();
            format!("a size of exactly {} bytes", list.join(", "))
        }
        Test::SizeAtMost(size) => format!("at most {size} bytes"),
        Test::Homogeneous { limit } => {
            format!("at most {limit} floating point members of one type and nothing else")
        }
        Test::FloatPair => "one or two members with a floating point member among them".to_string(),
        Test::X87Stack => "one x87 long double, or two if it is a _Complex".to_string(),
        Test::Eightbytes { limit } => {
            format!("at most {limit} bytes that classify into eightbytes")
        }
    };
    let then = match rule.then {
        Travel::Ignore => "travels nowhere",
        Travel::AsFound => "travels in the registers the rule found",
        Travel::AsIntegers => "travels in integer registers, one per register width",
        Travel::AsOneInteger => "travels in one integer register of its exact size",
        Travel::ByReference => "travels as the address of a copy",
        Travel::InMemory => "travels in the argument area",
    };
    let short = match rule.short {
        Short::Unchanged => "",
        Short::Memory => ", and short of registers goes to the argument area",
        Short::MemoryAndDrain => {
            ", and short of registers goes to the argument area with the rest of that bank"
        }
        Short::TryNextRule => ", and short of registers falls through to the rule below",
    };
    format!("{when} {then}{short}")
}
