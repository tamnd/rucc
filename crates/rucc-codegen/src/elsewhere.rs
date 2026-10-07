//! Which names this file may not work the address of out for itself.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.3.
//!
//! Everything this compiler emits is position independent, so the address of a name is the distance
//! from the instruction asking to the name, and that distance is a number the assembler leaves a
//! hole for and the linker fills in. The linker can only fill it in when it is putting both ends in
//! the same program. A name this file only declares may turn out to be in a shared library, and
//! then there is no such distance and the link fails rather than guessing one.
//!
//! The way round it is a table: the linker gives the name one slot in the global offset table, fills
//! the slot with whatever address the name ends up at, and the code loads the address out of the
//! slot instead of working it out. The slot is in this program, so the distance to the slot is a
//! number the linker has. It costs a load, and the linker takes the load back out again when the
//! name turns out to have been in this program all along.
//!
//! Which names need it is a fact about the whole module and the code generator sees one function at
//! a time, which is why this is worked out first and handed in rather than asked at the point of
//! use.
//!
//! It is also a fact about which link is coming, which is [`rucc_ir::Pic`] and is why this is built
//! from more than the module. Under `-fPIC` the link may be one that produces a shared library, and
//! then a name this file exports is one the dynamic linker may find a different definition of, so
//! reaching it from the instruction pointer would reach the wrong one. The static linker will not
//! let that happen quietly: `R_X86_64_PC32` against a name it can see is replaceable is refused
//! when it is making a shared object, which is how tamnd/rucc#756 was found.
//!
//! A thread-local variable is the other name this file cannot work the address of out for itself,
//! and it is here for the same reason: which names are thread-local is a fact about the module and
//! the code generator sees one function at a time. It is a harder case than the one above rather
//! than a variation of it, because there is no address to work out at all. Every thread has its own
//! copy, so what the link can say is only where the variable sits inside the block a thread gets,
//! and turning that into an address is something the running program does. See [`Elsewhere::thread`].
//!
//! COFF has no global offset table and answers the same question with pointers of its own, one per
//! name, which is [`Elsewhere::slot`]. A name a declaration said is in another DLL is reached
//! through the pointer the loader fills in for it, and a variable this file only declares is
//! reached through a pointer the file writes itself, so that the link may send it to a DLL without
//! this file having known.

use rucc_base::Symbol;
use rucc_base::hash::{Map, Set};
use rucc_ir::{AttrSet, Datum, Dll, Extra, Linkage, Module, Opcode, Pic, Visibility};
use rucc_target::ObjectFormat;

/// The names whose address only the linker knows.
///
/// Two ways in, and the first one holds whichever link is coming. A function this file only
/// declares is one, because a function cannot be copied: it has exactly one address that every
/// object in the program has to agree on, or two pointers to it compare unequal, so the one address
/// is what the table holds and what everything reads. A variable can be copied, and in an
/// executable it is, since the linker answers a reference to one another object defines by making
/// room for it here and copying it there, so the name really does end up somewhere this file can
/// measure to.
///
/// The second way in is `-fPIC`, where the link may be one that produces a shared library and the
/// copying does not happen. There every replaceable name is in here, defined or not and function or
/// variable, because the definition the process ends up using may be in another object however
/// plainly this file defines it. What is not in here is what `-fPIC` costs nothing for: a `static`,
/// and a name marked hidden or protected, which is the reason `-fPIC -fvisibility=hidden` is the
/// combination a library that cares about its own speed is built with.
///
/// Both ways in are shut on a format with no such table, which is COFF. See `Self::table` for why
/// the question has a different answer there rather than no answer.
///
/// A name this module has never heard of is not in here. Nothing the front end writes produces one,
/// and treating an unknown name as a function would put the addresses the instrumentation takes of
/// its own tables through a table of their own for no reason.
///
/// A thread-local variable is kept separately and answered by [`Self::thread`], because the two
/// questions have different answers rather than one being a case of the other: the table slot of an
/// ordinary name holds its address and the slot of a thread-local holds an offset, and reading
/// either as though it were the other is a wrong answer rather than a slower one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Elsewhere {
    names: Set<Symbol>,
    threads: Set<Symbol>,
    twice: Set<Symbol>,
    jumps_back: Set<Symbol>,
    cold: Set<Symbol>,
    described: bool,
    indexed: bool,
    imported: Set<Symbol>,
    referred: Set<Symbol>,
    based: bool,
    defined: Set<Symbol>,
    aligned: Map<Symbol, u32>,
    unstubbed: Set<Symbol>,
}

/// Which pointer a name on COFF is reached through, when it is reached through one.
///
/// The code is the same for both, a load of the pointer from the instruction pointer and then the
/// name's address in a register. What differs is who writes the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// The one the loader fills in for a name in another DLL, which the import library calls
    /// `__imp_` and the name. A declaration said `dllimport`, so there is no other way to the name:
    /// the import library has no stub under the plain name for a variable, and for a function the
    /// stub is a jump through this same pointer, so going through it here saves the jump.
    Imported,
    /// One this file writes itself, called `.refptr.` and the name, for a variable it only
    /// declares and nothing said was in a DLL.
    ///
    /// The variable may still turn out to be in one, and `environ` in the C runtime is one that
    /// is. Then the address a `lea` would work out is not a distance the linker has, since the DLL
    /// is loaded wherever it fits, and what it does instead is write a record for the runtime to
    /// patch the reference with once the DLL is loaded. A four byte reference from the code cannot
    /// hold an address that far away, and an eight byte pointer in a data section can, so the
    /// reference the runtime patches is this pointer. Every object that reads the name writes the
    /// same one in a section of its own that the linker keeps one copy of, which is what gcc and
    /// clang both do for `x86_64-w64-mingw32`.
    Referred,
}

impl Slot {
    /// The pointer's own name, for a pointer to `name`.
    #[must_use]
    pub fn name(self, name: &str) -> String {
        match self {
            Self::Imported => format!("__imp_{name}"),
            Self::Referred => format!(".refptr.{name}"),
        }
    }
}

impl Elsewhere {
    /// The names that link cannot reach from the instruction pointer.
    ///
    /// `copies` is whether the linker answers a reference from the instruction pointer to a
    /// variable another object defines by copying the variable into the executable. x86-64 does,
    /// even in a position independent executable. AArch64 and RISC-V do not: GNU ld refuses an
    /// `adrp` against such a variable when it makes a PIE, which is the default link on every
    /// distribution, and gcc reads the address out of the table there instead.
    #[must_use]
    pub fn of(module: &Module, pic: Pic, format: ObjectFormat, copies: bool) -> Self {
        let threads = module
            .globals()
            .filter(|&id| module[id].tls.is_some())
            .map(|id| module[id].name)
            .collect();
        let twice = module
            .funcs()
            .filter(|&id| module[id].attrs.set.contains(AttrSet::RETURNS_TWICE))
            .map(|id| module[id].name)
            .collect();
        let jumps_back = module
            .funcs()
            .filter(|&id| module[id].attrs.set.contains(AttrSet::INDIRECT_RETURN))
            .map(|id| module[id].name)
            .collect();
        let cold = module
            .funcs()
            .filter(|&id| module[id].attrs.set.contains(AttrSet::COLD))
            .map(|id| module[id].name)
            .collect();
        let described = format == ObjectFormat::MachO;
        let indexed = format == ObjectFormat::Coff;
        let (imported, referred) = Self::pointers(module, format);
        let aligned = if format == ObjectFormat::Elf {
            module.globals().map(|id| (module[id].name, module[id].align)).collect()
        } else {
            Map::default()
        };
        Self {
            threads,
            twice,
            jumps_back,
            cold,
            described,
            indexed,
            imported,
            referred,
            aligned,
            ..Self::table(module, pic, format, copies)
        }
    }

    /// The names COFF reaches through a pointer, as the ones a declaration said are in another DLL
    /// and the ones this file writes a pointer to itself. Both are empty on every other format.
    ///
    /// A variable gets a pointer of this file's own when it is only declared here, the linker may
    /// see it, and nothing said where it is. A thread-local is left out, since it has no address
    /// to point at and [`Self::thread`] answers for it. So is a hidden one, which is promised to be
    /// in this image: clang reaches that one directly and so does this, where gcc still goes
    /// through a pointer for it. A function never gets one from this file, since the import
    /// library's stub under the plain name is already an address in this image, and neither
    /// compiler writes one for a function either.
    fn pointers(module: &Module, format: ObjectFormat) -> (Set<Symbol>, Set<Symbol>) {
        if format != ObjectFormat::Coff {
            return (Set::default(), Set::default());
        }
        let funcs = module
            .funcs()
            .filter(|&id| module[id].is_declaration() && module[id].dll == Dll::Import)
            .map(|id| module[id].name);
        let globals = module
            .globals()
            .filter(|&id| module[id].is_declaration() && module[id].tls.is_none())
            .filter(|&id| module[id].dll == Dll::Import)
            .map(|id| module[id].name);
        let referred = module
            .globals()
            .filter(|&id| {
                let global = &module[id];
                global.is_declaration()
                    && global.tls.is_none()
                    && global.dll != Dll::Import
                    && global.visibility == Visibility::Default
                    && matches!(global.linkage, Linkage::External | Linkage::Weak)
            })
            .map(|id| module[id].name)
            .collect();
        (funcs.chain(globals).collect(), referred)
    }

    /// The half of the above that is about the global offset table, which is the older one.
    ///
    /// Empty on a format that has no such table. COFF is the one, and it is not that the question
    /// goes unanswered there: a name this file only declares is reached from the instruction
    /// pointer like any other, because whatever supplies it supplies a piece of this image to
    /// measure to. A name the link resolves out of another object is in the image, and a name that
    /// comes from a DLL arrives through an import library, which is an archive member holding a
    /// jump under the plain name, so the name still stands for an address in this image and every
    /// object that takes it gets the one the linker kept. Measured against gcc 13.2 for
    /// `x86_64-w64-mingw32`, which writes `leaq other(%rip), %rax` for the address of a function it
    /// has only seen declared. Asking for a table there instead reached the object writer as a
    /// relocation it has no way to write, which is what tamnd/rucc#1443 was.
    ///
    /// A variable has no stub to stand for it, so one this file only declares is reached through a
    /// pointer instead, and so is anything a declaration said is in a DLL. Neither is a table the
    /// linker builds, which is why they are [`Self::slot`] and not in here.
    fn table(module: &Module, pic: Pic, format: ObjectFormat, copies: bool) -> Self {
        if format == ObjectFormat::Coff {
            return Self::default();
        }
        // Position dependent code, which is `-fno-pic` and a kernel. The link puts every name at
        // an address it knows and nothing moves the program afterwards, so there is no name this
        // file cannot reach directly, and the table is empty. A function this file only declares
        // gets the address the linker gives it, which in an executable is the canonical entry
        // it makes for it. A weak variable nothing defines is resolved to zero where it is
        // referenced, which `ld` does for `R_X86_64_PC32` and for the absolute forms alike. That
        // second one is why this is not only an optimization: a slot the linker cannot relax
        // leaves a `.got` behind, and the kernel's link script asserts there is none, so a single
        // `__start_` symbol read out of a slot is a kernel that does not link. tamnd/rucc#2276.
        //
        // Only where the linker copies variables, which on ELF is every machine once the code is
        // not position independent. Anywhere else the same code as an executable is what this
        // answers, and it is right.
        //
        // Except for a name a declaration said `nodirect_extern_access` of, which is a library's
        // promise that the name is in the library and stays there, protected, so it is never
        // copied and its address is read out of a slot even here, as gcc does. A hidden one is
        // in this image whatever was said, and gcc reaches it directly.
        let indirect = module
            .funcs()
            .filter(|&id| {
                let func = &module[id];
                func.is_declaration()
                    && func.attrs.set.contains(AttrSet::NODIRECT)
                    && func.visibility == Visibility::Default
            })
            .map(|id| module[id].name);
        let indirect = indirect.chain(
            module
                .globals()
                .filter(|&id| {
                    let global = &module[id];
                    global.is_declaration()
                        && global.indirect
                        && global.tls.is_none()
                        && global.visibility == Visibility::Default
                })
                .map(|id| module[id].name),
        );
        if pic == Pic::Absolute && copies && format == ObjectFormat::Elf {
            return Self { names: indirect.collect(), ..Self::default() };
        }
        // A declared function marked hidden or protected is promised to be in this image, so the
        // distance to it is one the linker has. The kernel's compressed loader is built `-fPIE`
        // under `#pragma GCC visibility push(hidden)` and its link script asserts there is no
        // `.got`, which a slot for `boot_page_fault` breaks. A weak one may still be nothing.
        let funcs = module.funcs().filter(|&id| {
            let func = &module[id];
            (func.is_declaration()
                && (func.visibility == Visibility::Default || func.linkage == Linkage::Weak))
                || pic.replaceable(func.linkage, func.visibility)
        });
        // A weak variable nothing here defines is the one variable the copying above does not
        // cover, since there may be no definition anywhere to copy and then its address is null. The
        // distance from here to null is not a number the linker has, so lld refuses the
        // `R_X86_64_PC32` and gcc reads the address out of a slot, which the linker fills with zero.
        //
        // Mach-O does no copying at all. `dyld` has no copy relocation, so a variable a library
        // defines stays in the library and the only way to it is the slot. That is every variable
        // this file only declares, unless it is hidden and so promised to be in the same image,
        // and it is what clang writes: `_ext@GOTPAGE` on arm64 and `_ext@GOTPCREL` on x86-64.
        let uncopied = format == ObjectFormat::MachO || !copies;
        let globals = module
            .globals()
            .filter(|&id| {
                let global = &module[id];
                (global.is_declaration()
                    && (global.linkage == Linkage::Weak
                        || (uncopied && global.visibility == Visibility::Default)))
                    || pic.replaceable(global.linkage, global.visibility)
            })
            .map(|id| module[id].name);
        // An alias is a symbol of its own with a linkage and a visibility of its own, so it answers
        // this for itself the same way it answered the visibility question in #752. What it points
        // at is a separate name and is decided separately, which is what `weak, alias,
        // visibility("hidden")` over an exported definition needs.
        let aliases = module
            .aliases()
            .filter(|&id| pic.replaceable(module[id].linkage, module[id].visibility))
            .map(|id| module[id].name);
        funcs.map(|id| module[id].name).chain(globals).chain(aliases).chain(indirect).collect()
    }

    /// Whether the file says it needs every name another object defines reached through the
    /// global offset table, which gcc says in a property of its own beside the feature word once a
    /// name it has said `nodirect_extern_access` of is defined here or used, hidden or not. A
    /// declaration nothing uses is not asked about, and gcc says nothing for it.
    ///
    /// The linker reads it to keep from copying a protected variable into an executable whose
    /// code may reach one directly, and the loader to refuse to put such an executable together
    /// with a library that was built expecting it would be.
    #[must_use]
    pub fn needs_indirect(module: &Module) -> bool {
        let said: Set<Symbol> = module
            .funcs()
            .filter(|&id| module[id].attrs.set.contains(AttrSet::NODIRECT))
            .map(|id| module[id].name)
            .chain(module.globals().filter(|&id| module[id].indirect).map(|id| module[id].name))
            .collect();
        if said.is_empty() {
            return false;
        }
        let defined = module
            .funcs()
            .filter(|&id| !module[id].is_declaration())
            .map(|id| module[id].name)
            .chain(
                module
                    .globals()
                    .filter(|&id| !module[id].is_declaration())
                    .map(|id| module[id].name),
            );
        let used = module.funcs().flat_map(|id| {
            let func = &module[id];
            func.blocks().flat_map(|block| func.insts(block)).filter_map(|inst| {
                match func[inst].extra {
                    Extra::Call(info) => func[info].callee,
                    Extra::Symbol(name) => Some(name),
                    _ => None,
                }
            })
        });
        let held = module.globals().flat_map(|id| {
            let init = module[id].init.map(|list| &module[list]).unwrap_or_default();
            init.iter().filter_map(|datum| match *datum {
                Datum::Addr(reloc) | Datum::Away(reloc) | Datum::Apart { to: reloc, .. } => {
                    Some(module[reloc].symbol)
                }
                _ => None,
            })
        });
        defined.chain(used).chain(held).any(|name| said.contains(&name))
    }

    /// How far the address of that variable is aligned, which is how many of its low bits are
    /// known to be zero. `None` for a name that is not a variable of this module, or on a format
    /// other than ELF, where nothing asks.
    ///
    /// A load from a variable on AArch64 is `adrp` and then the low twelve bits of the address in
    /// the load itself, and the load scales those bits by the size it reads, so the address has to
    /// be a multiple of that size. A variable another object defines is as aligned as its type
    /// says here, which is what gcc takes it to be too.
    #[must_use]
    pub fn aligned(&self, name: Symbol) -> Option<u32> {
        self.aligned.get(&name).copied()
    }

    /// Whether the address of that name has to be read out of the global offset table.
    #[must_use]
    pub fn holds(&self, name: Symbol) -> bool {
        self.names.contains(&name)
    }

    /// The pointer the address of that name is read out of on COFF, where it is read out of one.
    ///
    /// Asked after [`Self::thread`] and in place of [`Self::holds`], which is never yes on the
    /// format this is ever yes on.
    #[must_use]
    pub fn slot(&self, name: Symbol) -> Option<Slot> {
        if self.imported.contains(&name) {
            Some(Slot::Imported)
        } else if self.referred.contains(&name) {
            Some(Slot::Referred)
        } else {
            None
        }
    }

    /// The names this file has to write a pointer of its own for, in the order the module has
    /// them, which are the ones [`Self::slot`] says are [`Slot::Referred`] and that some function
    /// here takes the address of.
    ///
    /// Asked of the module once its functions have been compiled, because the question is which
    /// references survived: a read the optimizer took out needs no pointer, and gcc and clang both
    /// write one only for a name the code still reads. A pointer nothing reads would not be free
    /// either, since it names the variable and so asks the link to find a definition of it.
    #[must_use]
    pub fn referred(&self, module: &Module) -> Vec<Symbol> {
        Self::read(module, &self.referred)
    }

    /// The variables a declaration said are in a DLL and that some function here still takes the
    /// address of, in the order the module has them, which are the ones read through
    /// [`Slot::Imported`].
    ///
    /// Asked once the functions have been compiled, for the reason [`Self::referred`] is. The
    /// driver hands these to the linker by name on Microsoft's side, which is tamnd/rucc#2182:
    /// when the static C runtime defines such a variable, `lld-link` makes the pointer itself and
    /// then throws the variable away as unreferenced, so the pointer holds the image base.
    #[must_use]
    pub fn imported_variables(&self, module: &Module) -> Vec<Symbol> {
        Self::read(module, &self.imported)
    }

    /// The variables among `names` whose address some function here takes.
    fn read(module: &Module, names: &Set<Symbol>) -> Vec<Symbol> {
        if names.is_empty() {
            return Vec::new();
        }
        let mut read = Set::default();
        for id in module.funcs() {
            let func = &module[id];
            for block in func.blocks() {
                for inst in func.insts(block) {
                    let data = &func[inst];
                    if let (Opcode::GlobalAddr, Extra::Symbol(name)) = (data.opcode, data.extra) {
                        read.insert(name);
                    }
                }
            }
        }
        module
            .globals()
            .map(|id| module[id].name)
            .filter(|name| names.contains(name) && read.contains(name))
            .collect()
    }

    /// The same set for AArch64 code that is not position independent, which still reads a weak
    /// name nothing here defines out of the table, as gcc does. The name may be nothing at all, and
    /// `adrp` cannot reach null from an image linked far above it, which a kernel always is. A
    /// hidden or protected one is promised to be in the image and is reached directly.
    #[must_use]
    pub fn weak_from_table(mut self, module: &Module) -> Self {
        let funcs = module
            .funcs()
            .filter(|&id| {
                let func = &module[id];
                func.is_declaration()
                    && func.linkage == Linkage::Weak
                    && func.visibility == Visibility::Default
            })
            .map(|id| module[id].name);
        let globals = module
            .globals()
            .filter(|&id| {
                let global = &module[id];
                global.is_declaration()
                    && global.linkage == Linkage::Weak
                    && global.visibility == Visibility::Default
                    && global.tls.is_none()
            })
            .map(|id| module[id].name);
        self.names.extend(funcs.chain(globals));
        self
    }

    /// The functions that a call reaches through the global offset table and not through a stub of
    /// the procedure linkage table, which is `-fno-plt`.
    ///
    /// These are the functions that can be in another object: a declared function that is not
    /// hidden or protected, and under `-fPIC` each function that another object can replace. The
    /// call reads the address from the slot and calls through it, `call *f@GOTPCREL(%rip)` on
    /// x86-64 and `adrp`, `ldr` and `blr` on AArch64, as gcc does. With `-z now` the loader fills
    /// each slot at startup, and no call goes through a stub.
    #[must_use]
    pub fn without_plt(mut self, module: &Module, pic: Pic) -> Self {
        self.unstubbed = module
            .funcs()
            .filter(|&id| {
                let func = &module[id];
                (func.is_declaration()
                    && (func.visibility == Visibility::Default || func.linkage == Linkage::Weak))
                    || pic.replaceable(func.linkage, func.visibility)
            })
            .map(|id| module[id].name)
            .collect();
        self
    }

    /// Whether a call to that name reads the address from the global offset table. See
    /// [`Self::without_plt`].
    #[must_use]
    pub fn unstubbed(&self, name: Symbol) -> bool {
        self.unstubbed.contains(&name)
    }

    /// The same set for i386 position independent code, which reaches every name from the global
    /// offset table's address in a register, since the machine has no addressing relative to the
    /// instruction pointer. The driver says so for i386 ELF whenever the code is not `-fno-pic`.
    ///
    /// Which functions this module defines is kept alongside, because a call to anything else goes
    /// through the procedure linkage table, and on i386 that is a call that needs the table's
    /// address in `%ebx`. See [`Self::linked`].
    #[must_use]
    pub fn based_on_table(mut self, module: &Module) -> Self {
        self.based = true;
        self.defined = module
            .funcs()
            .filter(|&id| !module[id].is_declaration())
            .map(|id| module[id].name)
            .collect();
        self
    }

    /// Whether every name is reached from the global offset table's address in a register, which
    /// is i386 position independent code. See [`Self::based_on_table`].
    #[must_use]
    pub const fn based(&self) -> bool {
        self.based
    }

    /// Whether a call to that name goes through the procedure linkage table, which is a call to a
    /// name this file does not define or one the link may replace, in i386 position independent
    /// code. A call to the runtime's own routines is one of them, since the names are not in the
    /// module at all. Never on any other target, whose calls need nothing said about them.
    #[must_use]
    pub fn linked(&self, name: Symbol) -> bool {
        self.based && (self.holds(name) || !self.defined.contains(&name))
    }

    /// Whether that name is a variable every thread has its own copy of.
    ///
    /// Asked before [`Self::holds`] and not instead of it, because the two answers are about
    /// different things: a thread-local variable that another object may define is still reached
    /// the same way, since the table slot holds an offset that is the same for every copy and the
    /// question of whose copy is answered by the segment register rather than by the link.
    #[must_use]
    pub fn thread(&self, name: Symbol) -> bool {
        self.threads.contains(&name)
    }

    /// Whether a call to that name may come back more than once, because a declaration of it said
    /// `returns_twice`.
    ///
    /// Not a question about addresses like the two above, but it is the same kind of fact: it is
    /// about the module, the function it changes is a different one from the function it is
    /// written on, and the code generator sees one function at a time. See
    /// [`crate::tail::comes_back`] for what the caller does with it.
    #[must_use]
    pub fn twice(&self, name: Symbol) -> bool {
        self.twice.contains(&name)
    }

    /// Whether a call to that name may come back by a jump rather than a `ret`, because its type
    /// said `indirect_return`. The call says so itself when it was made through that type, and
    /// this is for one that was not: a call through a pointer the optimizer found the target of,
    /// or one made before the declaration that said it, which gcc also reads off the callee.
    #[must_use]
    pub fn jumps_back(&self, name: Symbol) -> bool {
        self.jumps_back.contains(&name)
    }

    /// Whether a declaration of that name said `cold`, which is a promise that a call to it is
    /// rarely made. The same kind of fact as [`Self::twice`], and read by [`crate::cold`].
    #[must_use]
    pub fn cold(&self, name: Symbol) -> bool {
        self.cold.contains(&name)
    }

    /// Whether a thread-local variable is reached by calling through its descriptor, which is how
    /// Mach-O does it on both architectures.
    ///
    /// The slot the table holds for such a variable is the address of the descriptor rather than an
    /// offset from the thread pointer, and the first word of the descriptor is a function that takes
    /// that address and gives back this thread's copy. So there is no thread pointer to add to,
    /// and the answer is the value the call returns.
    #[must_use]
    pub const fn described(&self) -> bool {
        self.described
    }

    /// Whether a thread-local variable is reached through the array of `.tls` copies a Windows
    /// thread keeps, which is how COFF does it. See `crate::select::Indexed`.
    #[must_use]
    pub const fn indexed(&self) -> bool {
        self.indexed
    }
}

/// The same set, written out by hand.
///
/// [`Elsewhere::of`] is how the driver builds one and is the only way a compilation does. This is
/// for a test that wants to lower one function and say what is outside the file without building a
/// module for it to be outside of.
impl FromIterator<Symbol> for Elsewhere {
    fn from_iter<T: IntoIterator<Item = Symbol>>(names: T) -> Self {
        Self { names: names.into_iter().collect(), ..Self::default() }
    }
}

impl Elsewhere {
    /// The same set with those names said to be thread-local, for a test that lowers one function.
    #[must_use]
    pub fn with_threads<T: IntoIterator<Item = Symbol>>(mut self, threads: T) -> Self {
        self.threads = threads.into_iter().collect();
        self
    }

    /// The same set with those names said to be `cold`, for a test that lowers one function.
    #[must_use]
    pub fn with_cold<T: IntoIterator<Item = Symbol>>(mut self, cold: T) -> Self {
        self.cold = cold.into_iter().collect();
        self
    }

    /// The same set with thread-locals reached through a descriptor, for a test that lowers one
    /// function the way Mach-O would.
    #[must_use]
    pub const fn with_descriptors(mut self) -> Self {
        self.described = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rucc_base::Interner;
    use rucc_ir::{
        Alias, Builder, Func, Global, InstData, Linkage, Signature, TlsModel, Visibility,
    };
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    /// A module with one of everything: a function with a body and one without, a variable with an
    /// image and one without, a `static`, a hidden export, an alias and a thread-local.
    fn module(names: &mut Interner) -> Module {
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let mut module = Module::new(names.intern("test.c"), &target);
        let mut defined = Func::new(names.intern("here"), Signature::new());
        defined.create_block();
        module.add_func(defined);
        module.add_func(Func::new(names.intern("exit"), Signature::new()));

        let mut kept = Global::new(names.intern("kept"), 4, 4);
        kept.init = Some(module.push_data(&[]));
        module.add_global(kept);
        module.add_global(Global::new(names.intern("away"), 4, 4));

        let mut quiet = Global::new(names.intern("quiet"), 4, 4);
        quiet.init = Some(module.push_data(&[]));
        quiet.linkage = Linkage::Internal;
        module.add_global(quiet);

        let mut shy = Global::new(names.intern("shy"), 4, 4);
        shy.init = Some(module.push_data(&[]));
        shy.visibility = Visibility::Hidden;
        module.add_global(shy);

        let mut own = Global::new(names.intern("own"), 4, 4);
        own.init = Some(module.push_data(&[]));
        own.tls = Some(TlsModel::GlobalDynamic);
        module.add_global(own);

        module.add_alias(Alias::new(names.intern("second"), names.intern("here")));
        module
    }

    #[test]
    fn a_variable_every_thread_has_its_own_copy_of_is_one() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf, true);
        assert!(elsewhere.thread(names.intern("own")));
    }

    /// The question the other five ask is a different question, and a variable that is not
    /// thread-local answering yes to this one would put an offset where an address belongs.
    #[test]
    fn an_ordinary_variable_is_not() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf, true);
        for name in ["kept", "away", "quiet", "shy", "here"] {
            assert!(!elsewhere.thread(names.intern(name)), "{name} was called thread-local");
        }
    }

    #[test]
    fn a_function_this_file_only_declares_is_reached_through_the_table() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf, true);
        assert!(elsewhere.holds(names.intern("exit")));
    }

    /// Hidden is a promise that the definition is in this image, which is what the kernel's
    /// compressed loader relies on to link with no `.got`. A weak one may be nothing at all.
    #[test]
    fn a_hidden_function_this_file_only_declares_is_not_unless_it_is_weak() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let mut near = Func::new(names.intern("near"), Signature::new());
        near.visibility = Visibility::Hidden;
        module.add_func(near);
        let mut maybe = Func::new(names.intern("maybe"), Signature::new());
        maybe.visibility = Visibility::Hidden;
        maybe.linkage = Linkage::Weak;
        module.add_func(maybe);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf, true);
        assert!(!elsewhere.holds(names.intern("near")));
        assert!(elsewhere.holds(names.intern("maybe")));
    }

    #[test]
    fn a_function_this_file_defines_is_not() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf, true);
        assert!(!elsewhere.holds(names.intern("here")));
    }

    #[test]
    fn a_name_the_module_does_not_carry_at_all_is_not() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf, true);
        assert!(!elsewhere.holds(names.intern("nowhere")));
    }

    /// The whole of what an executable pays, which is one entry for the one function it calls in a
    /// library. Every variable is reached from the instruction pointer, the one it does not define
    /// included, because the linker copies that one in here.
    #[test]
    fn an_executable_pays_for_the_functions_and_for_nothing_else() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf, true);
        for name in ["kept", "away", "quiet", "shy", "second"] {
            assert!(!elsewhere.holds(names.intern(name)), "{name} was in the table");
        }
    }

    /// A weak variable nothing defines may be at zero, which no distance from the code reaches.
    #[test]
    fn a_weak_variable_this_file_only_declares_is_reached_through_the_table() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let mut maybe = Global::new(names.intern("maybe"), 4, 4);
        maybe.linkage = Linkage::Weak;
        module.add_global(maybe);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf, true);
        assert!(elsewhere.holds(names.intern("maybe")));
    }

    /// Position dependent code reaches every name directly, the weak variable nothing may define
    /// and the function this file only declares included. A kernel's link script asserts that
    /// there is no `.got`, and one slot for a `__start_` symbol is enough to make one.
    #[test]
    fn position_dependent_code_puts_nothing_in_the_table() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let mut maybe = Global::new(names.intern("maybe"), 4, 4);
        maybe.linkage = Linkage::Weak;
        module.add_global(maybe);
        let elsewhere = Elsewhere::of(&module, Pic::Absolute, ObjectFormat::Elf, true);
        for name in ["here", "exit", "kept", "away", "quiet", "shy", "second", "maybe"] {
            assert!(!elsewhere.holds(names.intern(name)), "{name} was in the table");
        }
        // Thread-local storage is a different question and keeps its answer.
        assert!(elsewhere.thread(names.intern("own")));
        // A linker that makes no copies keeps the executable's answer, since the driver never
        // sends this for one and the executable's code is still right there.
        let uncopied = Elsewhere::of(&module, Pic::Absolute, ObjectFormat::Elf, false);
        assert!(uncopied.holds(names.intern("away")));
        assert!(uncopied.holds(names.intern("maybe")));
    }

    /// AArch64 without pic reaches every name directly but a weak one nothing here defines, which
    /// gcc still reads out of a slot there, and not a hidden one, which is in the image.
    #[test]
    fn aarch64_position_dependent_code_reads_only_the_weak_ones_out_of_the_table() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let mut maybe = Global::new(names.intern("maybe"), 4, 4);
        maybe.linkage = Linkage::Weak;
        module.add_global(maybe);
        let mut perhaps = Func::new(names.intern("perhaps"), Signature::new());
        perhaps.linkage = Linkage::Weak;
        module.add_func(perhaps);
        let mut inside = Global::new(names.intern("inside"), 4, 4);
        inside.linkage = Linkage::Weak;
        inside.visibility = Visibility::Hidden;
        module.add_global(inside);
        let elsewhere =
            Elsewhere::of(&module, Pic::Absolute, ObjectFormat::Elf, true).weak_from_table(&module);
        assert!(elsewhere.holds(names.intern("maybe")));
        assert!(elsewhere.holds(names.intern("perhaps")));
        for name in ["here", "exit", "kept", "away", "quiet", "shy", "inside"] {
            assert!(!elsewhere.holds(names.intern(name)), "{name} was in the table");
        }
    }

    /// Mach-O never copies a variable into the executable, so the one this file only declares is
    /// read through the table even in a program, and the ones it defines are still reached
    /// directly.
    #[test]
    fn a_mach_o_executable_pays_for_the_variables_it_does_not_define_as_well() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let mut near = Global::new(names.intern("near"), 4, 4);
        near.visibility = Visibility::Hidden;
        module.add_global(near);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::MachO, true);
        assert!(elsewhere.holds(names.intern("away")));
        for name in ["kept", "quiet", "shy", "near"] {
            assert!(!elsewhere.holds(names.intern(name)), "{name} was in the table");
        }
    }

    /// An AArch64 executable pays for a variable it only declares, because the linker there makes
    /// no copy for an `adrp` and refuses one in a PIE. bzip2 reading `stderr` is what found it.
    #[test]
    fn an_executable_that_gets_no_copies_pays_for_the_variables_it_does_not_define() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf, false);
        assert!(elsewhere.holds(names.intern("away")));
        for name in ["kept", "quiet", "shy"] {
            assert!(!elsewhere.holds(names.intern(name)), "{name} was in the table");
        }
        let copied = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf, true);
        assert!(!copied.holds(names.intern("away")));
    }

    /// A library pays for every name it exports, defined here or not, because the definition the
    /// process uses may be in another object however plainly this file defines it.
    #[test]
    fn a_library_pays_for_every_name_something_else_may_define() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Library, ObjectFormat::Elf, true);
        for name in ["here", "exit", "kept", "away", "second"] {
            assert!(elsewhere.holds(names.intern(name)), "{name} was not in the table");
        }
    }

    /// A format with no table asks nothing of anybody, which is not the same as asking and being
    /// told no. The name of a function this file only declares stands for an address in the image
    /// on this format whether the link finds it in another object or in an import library, so the
    /// instruction pointer reaches it and there is nothing left over to put in a table. gcc writes
    /// the same `leaq other(%rip)` for the same declaration.
    #[test]
    fn a_format_with_no_table_puts_nothing_in_one() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Coff, true);
        for name in ["here", "exit", "kept", "away", "quiet", "shy", "second"] {
            assert!(!elsewhere.holds(names.intern(name)), "{name} was in the table");
        }
    }

    /// And the flag that fills the table on the other format does not fill it here either, since
    /// there is no interposition on this one for it to be about.
    #[test]
    fn a_format_with_no_table_does_not_grow_one_under_the_library_flag() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Library, ObjectFormat::Coff, true);
        for name in ["here", "exit", "kept", "away", "second"] {
            assert!(!elsewhere.holds(names.intern(name)), "{name} was in the table");
        }
    }

    /// The other question this type answers is not the table's, so it keeps its answer whatever the
    /// format. What a target with no thread-local storage does about it is the writer's refusal
    /// rather than a name quietly left out here.
    #[test]
    fn a_format_with_no_table_still_says_which_variable_every_thread_has_a_copy_of() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Coff, true);
        assert!(elsewhere.thread(names.intern("own")));
    }

    /// And not for the names nothing outside can reach, which is what makes `-fvisibility=hidden`
    /// worth writing next to it.
    #[test]
    fn a_library_pays_nothing_for_a_name_nothing_outside_it_can_see() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Library, ObjectFormat::Elf, true);
        assert!(!elsewhere.holds(names.intern("quiet")));
        assert!(!elsewhere.holds(names.intern("shy")));
    }

    /// The module above with the names a Windows program has: a function and a variable a
    /// declaration said are in a DLL, a variable it only declares and one it said is hidden, and
    /// a function that reads every variable in it.
    fn windows(names: &mut Interner) -> Module {
        let mut module = module(names);
        let mut pid = Func::new(names.intern("GetCurrentProcessId"), Signature::new());
        pid.dll = Dll::Import;
        module.add_func(pid);
        let mut mode = Global::new(names.intern("_fmode"), 4, 4);
        mode.dll = Dll::Import;
        module.add_global(mode);
        let mut near = Global::new(names.intern("near"), 4, 4);
        near.visibility = Visibility::Hidden;
        module.add_global(near);
        let mut reader = Func::new(names.intern("reader"), Signature::new());
        let block = reader.create_block();
        for name in ["kept", "away", "quiet", "_fmode", "near"] {
            let symbol = names.intern(name);
            let data =
                InstData { extra: Extra::Symbol(symbol), ..InstData::new(Opcode::GlobalAddr) };
            Builder::new(&mut reader, block).value(data, rucc_ir::Type::PTR);
        }
        module.add_func(reader);
        module
    }

    /// What a declaration said is in a DLL is reached through the pointer the loader fills in,
    /// whether it is a function or a variable, and what it said nothing about keeps the plain name
    /// for a function and gets a pointer of the file's own for a variable.
    #[test]
    fn a_name_in_a_dll_is_reached_through_the_pointer_the_loader_fills_in() {
        let mut names = Interner::new();
        let module = windows(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Coff, true);
        for name in ["GetCurrentProcessId", "_fmode"] {
            assert_eq!(elsewhere.slot(names.intern(name)), Some(Slot::Imported), "{name}");
        }
        assert_eq!(elsewhere.slot(names.intern("away")), Some(Slot::Referred));
        for name in ["exit", "here", "kept", "quiet", "shy", "own", "near"] {
            assert_eq!(elsewhere.slot(names.intern(name)), None, "{name}");
        }
        assert_eq!(Slot::Imported.name("_fmode"), "__imp__fmode");
        assert_eq!(Slot::Referred.name("away"), ".refptr.away");
    }

    /// A pointer of the file's own is written only for a name some function still reads, so
    /// `unread`, which is declared the way `away` is and read by nothing, gets none.
    #[test]
    fn a_pointer_is_written_only_for_a_variable_the_code_reads() {
        let mut names = Interner::new();
        let mut module = windows(&mut names);
        module.add_global(Global::new(names.intern("unread"), 4, 4));
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Coff, true);
        assert_eq!(elsewhere.referred(&module), vec![names.intern("away")]);
    }

    /// Every other format has a table of its own and never asks for either pointer.
    #[test]
    fn a_format_with_a_table_has_no_pointers() {
        let mut names = Interner::new();
        let module = windows(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf, true);
        for name in ["GetCurrentProcessId", "_fmode", "away"] {
            assert_eq!(elsewhere.slot(names.intern(name)), None, "{name}");
        }
        assert!(elsewhere.referred(&module).is_empty());
    }

    /// A name a declaration said `nodirect_extern_access` of is read out of the table in position
    /// dependent code as well, unless it is hidden, and a file that only declares it and never
    /// uses it says nothing about needing that.
    #[test]
    fn a_name_kept_from_direct_access_is_read_out_of_the_table_everywhere() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let mut far = Global::new(names.intern("far"), 4, 4);
        far.indirect = true;
        module.add_global(far);
        let mut near = Global::new(names.intern("near"), 4, 4);
        near.indirect = true;
        near.visibility = Visibility::Hidden;
        module.add_global(near);
        let mut called = Func::new(names.intern("called"), Signature::new());
        called.attrs.set |= AttrSet::NODIRECT;
        module.add_func(called);
        for pic in [Pic::Absolute, Pic::Executable, Pic::Library] {
            let elsewhere = Elsewhere::of(&module, pic, ObjectFormat::Elf, true);
            assert!(elsewhere.holds(names.intern("far")), "{pic:?}");
            assert!(elsewhere.holds(names.intern("called")), "{pic:?}");
            assert!(!elsewhere.holds(names.intern("near")), "{pic:?}");
        }
        // Not anything else, which is still reached directly in position dependent code.
        let elsewhere = Elsewhere::of(&module, Pic::Absolute, ObjectFormat::Elf, true);
        assert!(!elsewhere.holds(names.intern("exit")));
        assert!(!Elsewhere::needs_indirect(&module));
    }
}
