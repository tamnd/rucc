//! What `-flto` keeps in an object besides the machine code, which is the module the code was made
//! from.
//!
//! Design: `spec/09-optimizer.md` section 9.8.
//!
//! The object holds its code as it always has, so a link that knows nothing about any of this, or
//! an archive that `ar` and `nm` read, gets an ordinary object. The module goes in a section of its
//! own beside it, as the IR's text form after the optimizer, which is what the back end was given
//! and what `--emit=ir` prints. A few lines in front of the module say what the command line
//! decided that the module does not: which compiler wrote it, whether the code had to be position
//! independent, and which extensions the unit was built for.
//!
//! The link reads them back. Every object on its line that kept a module this version wrote gives
//! way to one object made from all of those modules joined, optimized as one unit and generated
//! again, and the linker is handed that one in the place of the first of them. An object in an
//! archive is not read, and neither is one another version wrote, so those link the code they
//! hold, as does every object when anything about the join goes wrong.

use std::path::Path;

use rucc_session::{EmitKind, OptLevel, Options, Pic, Safety, SaveTemps};
use rucc_target::Isa;

use crate::link::Item;

/// The section the module goes in.
pub const SECTION: &str = ".rucc.lto";

/// The first line of the section, and the version of what follows it. A link that finds another
/// one has a module it cannot read and links the code beside it instead.
const MAGIC: &str = "rucc-lto 1";

/// What an object keeps for the link, read back out of its section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept<'a> {
    /// The version of the compiler that wrote it. The text form is this compiler's own and can
    /// change between versions, so a link only reads a module its own version wrote.
    pub version: &'a str,
    /// What `-fPIC` and its relatives said when the unit was compiled.
    pub pic: Pic,
    /// The extensions the unit was built for, which a function from it keeps when it is generated
    /// in a link built for fewer.
    pub isa: Isa,
    /// The module, as text.
    pub module: &'a str,
}

/// The contents of the section for a unit whose module printed as `module`.
#[must_use]
pub fn keep(module: &str, opts: &Options) -> Vec<u8> {
    let pic = match opts.pic {
        Pic::Executable => "executable",
        Pic::Library => "library",
        Pic::Absolute => "absolute",
    };
    let head = format!("{MAGIC}\nversion {}\npic {pic}\nisa {}\n\n", crate::VERSION, opts.isa);
    let mut out = Vec::with_capacity(head.len() + module.len());
    out.extend_from_slice(head.as_bytes());
    out.extend_from_slice(module.as_bytes());
    out
}

/// The section [`keep`]'s bytes went in, in the bytes of an object, or nothing for an object that
/// has none.
#[must_use]
pub fn kept(object: &[u8]) -> Option<&[u8]> {
    rucc_object::carried(object, SECTION)
}

/// What [`keep`] wrote, or why it is not something it wrote.
///
/// # Errors
///
/// When the bytes are not text, start with something other than this version of the section,
/// or leave out a line it needs.
pub fn read(bytes: &[u8]) -> Result<Kept<'_>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the module is not text".to_string())?;
    let (head, module) =
        text.split_once("\n\n").ok_or_else(|| "the module has no end to its header".to_string())?;
    let mut lines = head.lines();
    if lines.next() != Some(MAGIC) {
        return Err(format!("the module does not start with `{MAGIC}`"));
    }
    let (mut version, mut pic, mut isa) = (None, None, None);
    for line in lines {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "version" => version = Some(value),
            "pic" => {
                pic = Some(match value {
                    "executable" => Pic::Executable,
                    "library" => Pic::Library,
                    "absolute" => Pic::Absolute,
                    other => return Err(format!("`{other}` is not a kind of code")),
                });
            }
            "isa" => isa = Some(value.parse::<Isa>()?),
            // A line a later version added, which says nothing this one needs.
            _ => {}
        }
    }
    Ok(Kept {
        version: version.ok_or_else(|| "the module does not say what wrote it".to_string())?,
        pic: pic.ok_or_else(|| "the module does not say how its code was built".to_string())?,
        isa: isa.ok_or_else(|| "the module does not say what it was built for".to_string())?,
        module,
    })
}

/// The link's line with the objects that kept a module replaced by one object made from all of
/// them, written into `dir`, or nothing when fewer than two objects kept one and there is nothing
/// to join.
///
/// The code is generated at the link's `-O` level, and at `-O2` when the link line has none,
/// which is what lld does too. It is position independent when the link makes a library or any
/// unit was compiled to be.
///
/// # Errors
///
/// When the modules do not join or the joined one does not compile, which the caller reports and
/// then links the objects as they are.
pub(crate) fn link(
    opts: &Options,
    shared: bool,
    items: &[Item],
    dir: &Path,
) -> Result<Option<Vec<Item>>, String> {
    let mut objects = Vec::new();
    for (at, item) in items.iter().enumerate() {
        let Item::File(path) = item else { continue };
        // A file that is not there is the linker's to report, in its own words.
        let Ok(bytes) = std::fs::read(path) else { continue };
        if kept(&bytes).is_some() {
            objects.push((at, path.as_str(), bytes));
        }
    }
    let mut units = Vec::with_capacity(objects.len());
    let mut taken = Vec::with_capacity(objects.len());
    let mut pics = Vec::with_capacity(objects.len());
    for (at, path, bytes) in &objects {
        // An object another version wrote, or one whose section does not read, links its code.
        let Some(Ok(unit)) = kept(bytes).map(read) else { continue };
        if unit.version != crate::VERSION {
            continue;
        }
        units.push(rucc_lto::Unit { name: path, module: unit.module, isa: unit.isa });
        taken.push(*at);
        pics.push(unit.pic);
    }
    if units.len() < 2 {
        return Ok(None);
    }

    let mut opts = opts.clone();
    opts.pic = if shared || pics.contains(&Pic::Library) {
        Pic::Library
    } else if pics.contains(&Pic::Executable) {
        Pic::Executable
    } else {
        Pic::Absolute
    };
    if !opts.opt_level.runs_optimizer() {
        opts.opt_level = OptLevel::O2;
    }
    opts.emit = EmitKind::Object;
    opts.debug_info = false;
    opts.save_temps = SaveTemps::No;
    opts.safety = Safety::Off;
    opts.profile_data.counts = None;
    let bytes = crate::compile::joined(&opts, &units)?;
    let out = dir.join("lto.o").display().to_string();
    std::fs::write(&out, bytes).map_err(|e| format!("{out}: {e}"))?;

    let mut line = Vec::with_capacity(items.len() + 1 - taken.len());
    for (at, item) in items.iter().enumerate() {
        if Some(&at) == taken.first() {
            line.push(Item::File(out.clone()));
        } else if !taken.contains(&at) {
            line.push(item.clone());
        }
    }
    Ok(Some(line))
}

#[cfg(test)]
mod tests {
    use super::{Kept, keep, read};
    use rucc_session::{Options, Pic};
    use rucc_target::Triple;

    #[test]
    fn what_is_kept_is_what_is_read() {
        let mut opts = Options::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        for pic in [Pic::Executable, Pic::Library, Pic::Absolute] {
            opts.pic = pic;
            let bytes = keep("module a\n\nfunc f\n", &opts);
            let kept = read(&bytes).expect("it reads back");
            assert_eq!(
                kept,
                Kept {
                    version: crate::VERSION,
                    pic,
                    isa: opts.isa,
                    module: "module a\n\nfunc f\n"
                }
            );
        }
    }

    #[test]
    fn something_else_is_not_read() {
        assert!(read(b"rucc-lto 2\nversion 0\n\nmodule").unwrap_err().contains("rucc-lto 1"));
        assert!(
            read(b"rucc-lto 1\nversion 0\npic sideways\nisa \n\n")
                .unwrap_err()
                .contains("sideways")
        );
        assert!(read(b"rucc-lto 1\nversion 0\n").unwrap_err().contains("no end"));
        assert!(read(b"rucc-lto 1\npic library\nisa \n\n").unwrap_err().contains("what wrote it"));
        assert!(read(&[0xff, 0xfe]).unwrap_err().contains("not text"));
    }
}
