//! Global variables as the image a file carries and the facts a linker needs about it.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1, which asks that the text path and the
//! binary path share one description so they cannot disagree. That is what this is for data: the
//! walk over a module's globals happens once, here, and what it produces is a list of pieces that
//! [`crate::att`] writes down as directives and [`Globals::image`] writes down as bytes. A `.long`
//! in a listing and the four bytes in the object beside it come from the same piece.
//!
//! # What a piece is
//!
//! As much of an image as one directive says. The four kinds are the four things C can put in an
//! initializer: a run of zeros, a run of literal bytes, one scalar, and the address of a symbol.
//! The first three are bytes the compiler knows and the fourth is a hole the linker fills, which
//! is the only reason data has relocations at all.
//!
//! Where a variable goes is worked out here too, from what the variable is rather than from
//! anything the object format says: a variable nothing writes through goes in a page the loader
//! can map read only, one whose image is all zeros goes in the section that carries no image, and
//! one the program named a section for goes where the program said. What those sections are
//! called is the format's business and is in [`crate::format`].
//!
//! # What is refused
//!
//! A thread-local variable. Reaching one is a call to `__tls_get_addr` or a load from the thread
//! pointer depending on the model, none of which the back end builds yet, so a file with one in it
//! is refused by name rather than written out as an ordinary variable that every thread would
//! share.

use rucc_base::Interner;
use rucc_ir::{Datum, GlobalId, Linkage, Module};
use rucc_object::{Binding, Data, Object, Place, Reference, Reloc};

use crate::Error;

/// Every variable a module defines, laid out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Globals {
    /// One entry per definition, in the order the module held them. A declaration is not here,
    /// because a file says nothing about a variable another file defines beyond the references
    /// that name it, and those are already in the text.
    pub vars: Vec<Variable>,
}

/// One global variable, as the pieces of its image and what the linker is told about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    /// Its name, as the C program spelled it. The underscore an Apple symbol carries is added
    /// when it is written down, because it is a fact about the object format and not about the
    /// variable.
    pub name: String,
    /// How many bytes it occupies, which the pieces add up to.
    pub size: u64,
    /// What it has to be aligned to, always a power of two.
    pub align: u64,
    /// Which section it goes in.
    pub place: Place,
    /// How the linker sees the name.
    pub binding: Binding,
    /// Its image, in order.
    pub pieces: Vec<Piece>,
}

/// As much of an image as one directive says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// That many zero bytes, which is the tail of a partly initialized array and the whole of a
    /// variable with no initializer.
    Zero(u64),
    /// Those literal bytes, which is what a string literal and anything already laid out is.
    Bytes(Vec<u8>),
    /// One number, in the byte order the module was built for, as many bytes wide as its type.
    Scalar(Vec<u8>),
    /// The address of a symbol, which is a hole this compiler leaves and the linker fills.
    Addr {
        /// Whose address it is, as the C program spelled it.
        symbol: String,
        /// What to add to that address. `&array[2]` is the address of `array` plus eight.
        addend: i64,
        /// How many bytes it occupies.
        bytes: u8,
    },
}

impl Piece {
    /// How many bytes it contributes to the image.
    #[must_use]
    pub fn size(&self) -> u64 {
        match self {
            Piece::Zero(bytes) => *bytes,
            Piece::Bytes(bytes) | Piece::Scalar(bytes) => bytes.len() as u64,
            Piece::Addr { bytes, .. } => u64::from(*bytes),
        }
    }
}

impl Globals {
    /// The image of every variable, and where in each one the linker has to write an address.
    ///
    /// A variable in a section that carries no image contributes its size and none of its bytes,
    /// which is what makes a program with a large zeroed array a small file.
    #[must_use]
    pub fn image(&self) -> Data {
        let mut data = Data::default();
        for var in &self.vars {
            let mut object = Object {
                name: var.name.clone(),
                bytes: Vec::new(),
                size: var.size,
                align: var.align,
                place: var.place.clone(),
                binding: var.binding,
                relocs: Vec::new(),
            };
            if matches!(var.place, Place::Zero | Place::Merged) {
                data.objects.push(object);
                continue;
            }
            for piece in &var.pieces {
                match piece {
                    Piece::Zero(bytes) => {
                        object.bytes.resize(object.bytes.len() + *bytes as usize, 0);
                    }
                    Piece::Bytes(bytes) | Piece::Scalar(bytes) => {
                        object.bytes.extend_from_slice(bytes);
                    }
                    Piece::Addr { symbol, addend, bytes } => {
                        // The bytes are left zero rather than holding anything, because a linker
                        // writes the whole hole from the addend and never reads what was there.
                        object.relocs.push(Reloc {
                            at: object.bytes.len(),
                            symbol: symbol.clone(),
                            kind: Reference::Address { bytes: *bytes },
                            addend: *addend,
                        });
                        object.bytes.resize(object.bytes.len() + usize::from(*bytes), 0);
                    }
                }
            }
            data.objects.push(object);
        }
        data
    }
}

/// Every variable a module defines, laid out.
///
/// # Errors
///
/// [`Error::Thread`] for a thread-local variable, which is a program this compiler is behind on
/// rather than a mistake, and [`Error::Image`] for a piece of an initializer nothing here can
/// write down. See [`Error`].
pub fn globals(module: &Module, names: &Interner) -> Result<Globals, Error> {
    let mut out = Globals::default();
    for id in module.globals() {
        if module[id].is_declaration() {
            continue;
        }
        out.vars.push(variable(module, names, id)?);
    }
    Ok(out)
}

/// One variable, laid out.
fn variable(module: &Module, names: &Interner, id: GlobalId) -> Result<Variable, Error> {
    let global = &module[id];
    let name = names.resolve(global.name).to_owned();
    if global.tls.is_some() {
        return Err(Error::Thread { name });
    }
    let init = global.init.expect("a definition has an image");

    let mut pieces = Vec::new();
    let mut written = 0;
    for datum in &module[init] {
        let piece = match *datum {
            Datum::Zero(bytes) => Piece::Zero(bytes),
            Datum::Bytes(range) => Piece::Bytes(module[range].to_vec()),
            Datum::Scalar { ty, value } => {
                if ty.lanes() != 1 {
                    let why = format!("a {ty} in an initializer");
                    return Err(Error::Image { name, why });
                }
                let bytes = usize::try_from(ty.bits().div_ceil(8)).expect("a scalar this wide");
                let mut image = module[value].bits().to_le_bytes()[..bytes].to_vec();
                if !module.datalayout.little_endian {
                    image.reverse();
                }
                Piece::Scalar(image)
            }
            Datum::Addr(idx) => {
                let reloc = module[idx];
                // Four and eight are the widths a machine has a relocation for and a directive
                // for. Anything else is a module nothing here produced and neither half of the
                // description could write down, so it is refused rather than rounded to one.
                let bytes = match reloc.size {
                    4 | 8 => reloc.size as u8,
                    size => {
                        let why = format!("an address {size} bytes wide");
                        return Err(Error::Image { name, why });
                    }
                };
                let symbol = names.resolve(reloc.symbol).to_owned();
                Piece::Addr { symbol, addend: reloc.addend, bytes }
            }
        };
        written += piece.size();
        pieces.push(piece);
    }
    // An image shorter than the variable is the rest of an array nothing initialized, which the
    // front end may leave off the end rather than write out as zeros it already said were there.
    if written < global.size {
        pieces.push(Piece::Zero(global.size - written));
    }

    let place = place(module, names, id, &pieces);
    let binding = match global.linkage {
        Linkage::Internal => Binding::Local,
        Linkage::Weak | Linkage::LinkOnce => Binding::Weak,
        Linkage::External | Linkage::Common => Binding::Global,
    };
    let size = global.size.max(written);
    Ok(Variable { name, size, align: u64::from(global.align), place, binding, pieces })
}

/// Which section a variable goes in.
///
/// The program's answer when it gave one, and otherwise worked out from what the variable is. A
/// tentative definition is asked of the linker rather than put anywhere, since the whole of what
/// it says is that the variable exists and that some other file may say so too.
fn place(module: &Module, names: &Interner, id: GlobalId, pieces: &[Piece]) -> Place {
    let global = &module[id];
    if let Some(section) = global.section {
        return Place::Named(names.resolve(section).to_owned());
    }
    if global.linkage == Linkage::Common {
        return Place::Merged;
    }
    if pieces.iter().all(|piece| matches!(piece, Piece::Zero(_))) {
        return Place::Zero;
    }
    if global.constant {
        return Place::ReadOnly;
    }
    Place::Written
}

#[cfg(test)]
mod tests {
    use super::*;

    use rucc_ir::{Global, Imm, Reloc as IrReloc, TlsModel, Type};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    /// A module for the one target every case here is written for.
    fn module(names: &mut Interner) -> Module {
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        Module::new(names.intern("t.c"), &target)
    }

    /// A four byte variable with that image.
    fn defined(module: &mut Module, names: &mut Interner, name: &str, data: &[Datum]) -> GlobalId {
        let list = module.push_data(data);
        let mut global = Global::new(names.intern(name), 4, 4);
        global.init = Some(list);
        module.add_global(global)
    }

    #[test]
    fn a_declaration_is_not_a_variable_this_file_defines() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        module.add_global(Global::new(names.intern("x"), 4, 4));
        defined(&mut module, &mut names, "y", &[Datum::Zero(4)]);
        let vars = globals(&module, &names).expect("a module of two globals").vars;
        assert_eq!(vars.iter().map(|var| var.name.as_str()).collect::<Vec<_>>(), ["y"]);
    }

    #[test]
    fn a_number_in_an_image_is_the_bytes_the_machine_reads_it_as() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let value = module.add_imm(Imm::int(258, Type::int(32)));
        defined(&mut module, &mut names, "x", &[Datum::Scalar { ty: Type::int(32), value }]);
        let vars = globals(&module, &names).expect("a module of one global").vars;
        assert_eq!(vars[0].pieces, [Piece::Scalar(vec![2, 1, 0, 0])]);
        // The low byte first, which is what this machine reads and is a fact about the module
        // rather than about the variable.
        assert_eq!(vars[0].pieces[0].size(), 4);
    }

    #[test]
    fn what_a_variable_is_decides_which_section_it_goes_in() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let value = module.add_imm(Imm::int(1, Type::int(32)));
        let scalar = Datum::Scalar { ty: Type::int(32), value };

        let zeroed = defined(&mut module, &mut names, "zeroed", &[Datum::Zero(4)]);
        let written = defined(&mut module, &mut names, "written", &[scalar]);
        let read_only = defined(&mut module, &mut names, "read_only", &[scalar]);
        module[read_only].constant = true;
        let named = defined(&mut module, &mut names, "named", &[scalar]);
        module[named].section = Some(names.intern(".init_array"));
        let merged = defined(&mut module, &mut names, "merged", &[Datum::Zero(4)]);
        module[merged].linkage = Linkage::Common;

        let vars = globals(&module, &names).expect("a module of five globals").vars;
        let places: Vec<&Place> = vars.iter().map(|var| &var.place).collect();
        assert_eq!(
            places,
            [
                &Place::Zero,
                &Place::Written,
                &Place::ReadOnly,
                &Place::Named(".init_array".to_owned()),
                &Place::Merged,
            ]
        );
        let _ = (zeroed, written);
    }

    #[test]
    fn the_rest_of_an_image_the_front_end_left_off_is_zeros() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let value = module.add_imm(Imm::int(7, Type::int(8)));
        let id =
            defined(&mut module, &mut names, "x", &[Datum::Scalar { ty: Type::int(8), value }]);
        module[id].size = 4;
        let vars = globals(&module, &names).expect("a module of one global").vars;
        assert_eq!(vars[0].pieces, [Piece::Scalar(vec![7]), Piece::Zero(3)]);
        assert_eq!(vars[0].size, 4);
    }

    #[test]
    fn a_variable_holding_an_address_is_a_hole_and_a_name_for_the_linker() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let reloc = module.add_reloc(IrReloc { symbol: names.intern("y"), addend: 16, size: 8 });
        let id = defined(&mut module, &mut names, "p", &[Datum::Addr(reloc)]);
        module[id].size = 8;
        let vars = globals(&module, &names).expect("a module of one global").vars;
        assert_eq!(vars[0].pieces, [Piece::Addr { symbol: "y".to_owned(), addend: 16, bytes: 8 }]);

        let data = Globals { vars }.image();
        assert_eq!(data.objects[0].bytes, vec![0; 8]);
        assert_eq!(
            data.objects[0].relocs,
            [Reloc {
                at: 0,
                symbol: "y".to_owned(),
                kind: Reference::Address { bytes: 8 },
                addend: 16,
            }]
        );
    }

    #[test]
    fn a_variable_in_a_section_that_carries_no_image_carries_its_size_and_nothing_else() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let id = defined(&mut module, &mut names, "x", &[Datum::Zero(4096)]);
        module[id].size = 4096;
        let data = globals(&module, &names).expect("a module of one global").image();
        assert_eq!(data.objects[0].place, Place::Zero);
        assert_eq!(data.objects[0].size, 4096);
        // The point of the section: a program with a large zeroed array is a small file.
        assert!(data.objects[0].bytes.is_empty());
    }

    #[test]
    fn the_linkage_a_variable_had_decides_how_the_linker_sees_the_name() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        for (index, (linkage, binding)) in [
            (Linkage::External, Binding::Global),
            (Linkage::Internal, Binding::Local),
            (Linkage::Weak, Binding::Weak),
            (Linkage::LinkOnce, Binding::Weak),
        ]
        .into_iter()
        .enumerate()
        {
            let name = format!("x{index}");
            let id = defined(&mut module, &mut names, &name, &[Datum::Zero(4)]);
            module[id].linkage = linkage;
            let vars = globals(&module, &names).expect("a module of globals").vars;
            assert_eq!(vars[index].binding, binding, "{linkage:?}");
        }
    }

    #[test]
    fn a_thread_local_variable_is_refused_rather_than_shared_between_every_thread() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let id = defined(&mut module, &mut names, "x", &[Datum::Zero(4)]);
        module[id].tls = Some(TlsModel::GlobalDynamic);
        let error = globals(&module, &names).expect_err("a thread-local variable");
        assert_eq!(error, Error::Thread { name: "x".to_owned() });
    }
}
