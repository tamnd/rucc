//! Turning a file of assembly on the command line into an object file.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.3 for where this sits, and
//! `spec/11-asm-objects-debug.md` section 11.1 for what does the work.
//!
//! Short, because everything hard about it is in the two crates below. `rucc-asm` reads the text
//! and `rucc-object` writes the file, and what is here is the part that has to know what a file is:
//! reading one, running the preprocessor over it first when its name says so, and giving back the
//! same [`Compiled`] a compilation gives back so that the three loops in the driver do not each
//! need a second shape of result to handle.
//!
//! A `.S` goes through the preprocessor and a `.s` does not, which is the only difference between
//! them and is decided by the plan rather than here. This is told which happened.

use rucc_session::{FileSystem, Options};

use crate::compile::{Artifact, Compiled, Temps};
use crate::preprocess::preprocess;

/// One file of assembly, as an object file.
///
/// `cpp` is whether the preprocessor runs over it first, which is what the plan says and what the
/// difference between `.S` and `.s` is.
#[must_use]
pub fn assemble(opts: &Options, name: &str, cpp: bool, fs: &dyn FileSystem) -> Compiled {
    let mut messages = Vec::new();
    let mut deps = Vec::new();
    let mut temps = Temps::default();
    let text = if cpp {
        let out = preprocess(opts, name, fs);
        messages.extend(out.messages.iter().cloned());
        deps = out.deps.clone();
        if out.failed() {
            return done(Artifact::Nothing, messages, 1, deps, temps);
        }
        // Under `-save-temps` this is the `.s` beside the `.S`, which is the file somebody looking
        // at a macro that expanded to the wrong directive wants to read.
        temps.preprocessed = Some(out.text.clone());
        out.text
    } else {
        match fs.read(std::path::Path::new(name)) {
            Ok(bytes) => match String::from_utf8(bytes.to_vec()) {
                Ok(text) => text,
                Err(_) => {
                    messages.push(format!("rucc: error: {name}: this is not text"));
                    return done(Artifact::Nothing, messages, 1, deps, temps);
                }
            },
            Err(e) => {
                messages.push(format!("rucc: error: {name}: {e}"));
                return done(Artifact::Nothing, messages, 1, deps, temps);
            }
        }
    };

    let assembled = match rucc_asm::read(&text) {
        Ok(assembled) => assembled,
        Err(trouble) => {
            // The same shape every other diagnostic in this compiler has, so that a build log
            // reads the same whether the file that stopped it was C or assembly and so that an
            // editor which jumps to a position finds this one too.
            messages.push(format!("{name}:{}: error: {}", trouble.line, trouble.why));
            return done(Artifact::Nothing, messages, 1, deps, temps);
        }
    };
    let defines = rucc_object::assembled_defines(&assembled);
    match rucc_object::assembled(&assembled, &rucc_target::TargetInfo::new(opts.target)) {
        Ok(bytes) => done(Artifact::Object { bytes, defines }, messages, 0, deps, temps),
        Err(e) => {
            messages.push(format!("rucc: error: {name}: {e}"));
            done(Artifact::Nothing, messages, 1, deps, temps)
        }
    }
}

/// One of these, with the fields nothing here fills in left as they are.
///
/// Most of [`Compiled`] is about the back end, and none of the back end runs for a file of
/// assembly. A rule this file fired or a register it spilled is not a fact about it, so those come
/// back empty, which is the right answer rather than a missing one: the caller unions them.
fn done(
    artifact: Artifact,
    messages: Vec<String>,
    errors: u32,
    deps: Vec<rucc_pp::Dependency>,
    temps: Temps,
) -> Compiled {
    Compiled {
        artifact,
        messages,
        errors,
        fired: rucc_codegen::coverage::Fired::new(),
        pressure: rucc_codegen::pressure::Pressure::new(),
        lowerings: rucc_codegen::lowering::Lowerings::new(),
        dumps: Vec::new(),
        remarks: String::new(),
        deps,
        temps,
    }
}
