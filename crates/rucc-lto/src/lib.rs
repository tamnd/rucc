//! Module merging, summaries and Thin LTO.
//!
//! Design: `spec/09-optimizer.md`. Layer rank 11, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! Merging is here: [`join`] reads the modules a link's objects kept into one module, which the
//! optimizer can then see whole. Summaries and Thin LTO are not.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.
//!
//! # Joining
//!
//! Each unit's module was written as if nothing else existed, and the linker is what would have
//! settled the rest. Joining has to settle it the way the linker would have:
//!
//! - A name with internal linkage belongs to its unit. Two units can both have a `static` called
//!   `count`, and every unit has a `.Lstr.0`, so a unit's internal name is renamed when an earlier
//!   unit already has it or any unit has it with linkage. The first unit to have it keeps it, so a
//!   program with no clash keeps every name it had. A unit with an `asm` at file scope is the
//!   exception, since the text may name its `static` and is not read here, so a clash in such a
//!   unit stops the join rather than renaming.
//! - A name with linkage is one thing however many units say it. Of a strong definition, a common
//!   one, a weak or once only one, and a declaration, the module keeps the strongest, and of two
//!   commons the larger, which is what the linker keeps. Two strong definitions are an error the
//!   linker would have given, and joining gives up so that it still does.
//! - A function keeps the extensions its unit was built for when the link is built for others,
//!   so that the inliner does not copy SSE4.2 code into a caller built without it.
//!
//! Nothing is made internal that was not. A program a library is loaded into, as Postgres loads
//! its extensions, can be asked for any symbol it has, and which ones are asked for is not
//! something a link can see.

#![doc(html_root_url = "https://docs.rs/rucc-lto/0.24.3")]

use rucc_base::hash::{Map, Set};
use rucc_base::{Interner, Symbol};
use rucc_ir::{Joiner, Linkage, Module, SymbolRef, Visibility};
use rucc_target::Isa;

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M8";

/// One unit's module, as an object kept it.
#[derive(Debug, Clone, Copy)]
pub struct Unit<'a> {
    /// What a message calls the unit, which is the object's file name.
    pub name: &'a str,
    /// The module, as the IR's text.
    pub module: &'a str,
    /// The extensions the unit was built for.
    pub isa: Isa,
}

/// What one unit has under one name.
#[derive(Debug, Clone, Copy)]
struct Item {
    unit: usize,
    kind: Kind,
    linkage: Linkage,
    visibility: Visibility,
    defined: bool,
    size: u64,
    align: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Func,
    Global,
    Alias,
}

impl Item {
    /// How strongly the item claims the name: a strong definition, then a common one, then a weak
    /// or once only one, then a declaration.
    fn rank(self) -> u8 {
        match self.linkage {
            Linkage::Common => 2,
            _ if !self.defined => 0,
            Linkage::External | Linkage::Internal => 3,
            Linkage::Weak | Linkage::LinkOnce => 1,
        }
    }
}

/// The units' modules as one module, for a link built for `isa`.
///
/// # Errors
///
/// When a module does not read, two are for different targets, two units define the same name
/// strongly, or one unit has a name as a function and another as a variable. Each is something
/// the link without joining either reports better or does not trip on.
pub fn join(units: &[Unit<'_>], isa: Isa, names: &mut Interner) -> Result<Module, String> {
    // Every module is read once on its own first, which is what says which names it defines and
    // how. The second reading is the one into the joined module, with that known.
    let mut alone = Vec::with_capacity(units.len());
    for unit in units {
        alone.push(rucc_ir::parse(unit.module, names).map_err(|e| format!("{}: {e}", unit.name))?);
    }

    let mut locals: Vec<Vec<Symbol>> = vec![Vec::new(); units.len()];
    let mut linked: Map<Symbol, Vec<Item>> = Map::default();
    // The names with linkage in the order the units have them, so that what a link with two
    // clashes reports does not depend on how a table hashed.
    let mut order: Vec<Symbol> = Vec::new();
    let mut taken: Set<Symbol> = Set::default();
    for (unit, module) in alone.iter().enumerate() {
        for (name, item) in items(module, unit) {
            taken.insert(name);
            if item.linkage == Linkage::Internal {
                locals[unit].push(name);
            } else {
                let list = linked.entry(name).or_default();
                if list.is_empty() {
                    order.push(name);
                }
                list.push(item);
            }
        }
        // The names of blocks whose address is taken, which are local symbols in the object
        // and are numbered from zero in every unit.
        for id in module.funcs() {
            for (_, name) in module[id].named_blocks() {
                taken.insert(name);
                locals[unit].push(name);
            }
        }
    }

    let mut renames: Vec<Map<Symbol, Symbol>> = vec![Map::default(); units.len()];
    let mut kept: Set<Symbol> = Set::default();
    for (unit, local) in locals.iter().enumerate() {
        for &name in local {
            if linked.contains_key(&name) || !kept.insert(name) {
                let fresh = fresh(name, unit, &mut taken, names);
                renames[unit].insert(name, fresh);
            }
        }
    }
    // An `asm` at file scope is text this does not read, and it may name a `static` by the name
    // the unit gave it. Renaming that one would leave the `asm` pointing at another unit's or at
    // nothing, so a unit with both is not joined.
    for (unit, module) in alone.iter().enumerate() {
        if !module.file_asms().is_empty() && !renames[unit].is_empty() {
            return Err(format!(
                "{}: an `asm` at file scope may name a local the join renames",
                units[unit].name
            ));
        }
    }

    let mut skips: Vec<Set<Symbol>> = vec![Set::default(); units.len()];
    let mut settled = Vec::with_capacity(order.len());
    for name in order {
        let list = &linked[&name];
        let winner = settle(name, list, units, names)?;
        for item in list {
            if item.unit != winner.unit {
                skips[item.unit].insert(name);
            }
        }
        settled.push((name, winner, mended(winner, list)));
    }

    let mut joiner = Joiner::new();
    for (index, unit) in units.iter().enumerate() {
        let before = joiner.module_mut().map_or(0, |module| module.counts().funcs);
        joiner
            .read(unit.module, names, &renames[index], &skips[index])
            .map_err(|e| format!("{}: {e}", unit.name))?;
        let Some(module) = joiner.module_mut() else {
            return Err(format!("{}: the module did not read", unit.name));
        };
        if unit.isa != isa {
            for id in module.funcs().skip(before) {
                let func = &mut module[id];
                if !func.is_declaration() && func.target.is_none() {
                    func.target = Some(unit.isa);
                }
            }
        }
    }
    let mut module = joiner.finish().ok_or_else(|| "there are no modules to join".to_string())?;

    for (name, winner, (linkage, visibility, align)) in settled {
        match module.lookup(name) {
            Some(SymbolRef::Func(id)) => {
                let func = &mut module[id];
                func.visibility = visibility;
                if !winner.defined {
                    func.linkage = linkage;
                }
            }
            Some(SymbolRef::Global(id)) => {
                let global = &mut module[id];
                global.visibility = visibility;
                global.align = global.align.max(align);
                if !winner.defined {
                    global.linkage = linkage;
                }
            }
            Some(SymbolRef::Alias(_)) | None => {}
        }
    }
    Ok(module)
}

/// Every function, global and alias a module has, by name.
fn items(module: &Module, unit: usize) -> Vec<(Symbol, Item)> {
    let mut out = Vec::new();
    for id in module.funcs() {
        let func = &module[id];
        out.push((
            func.name,
            Item {
                unit,
                kind: Kind::Func,
                linkage: func.linkage,
                visibility: func.visibility,
                defined: !func.is_declaration(),
                size: 0,
                align: 0,
            },
        ));
    }
    for id in module.globals() {
        let global = &module[id];
        out.push((
            global.name,
            Item {
                unit,
                kind: Kind::Global,
                linkage: global.linkage,
                visibility: global.visibility,
                defined: !global.is_declaration(),
                size: global.size,
                align: global.align,
            },
        ));
    }
    for id in module.aliases() {
        let alias = &module[id];
        out.push((
            alias.name,
            Item {
                unit,
                kind: Kind::Alias,
                linkage: alias.linkage,
                visibility: alias.visibility,
                defined: true,
                size: 0,
                align: 0,
            },
        ));
    }
    out
}

/// The item the joined module keeps for a name with linkage.
fn settle(
    name: Symbol,
    list: &[Item],
    units: &[Unit<'_>],
    names: &Interner,
) -> Result<Item, String> {
    let spelled = names.resolve(name);
    let mut best = list[0];
    for &item in &list[1..] {
        if matches!((best.kind, item.kind), (Kind::Func, Kind::Global) | (Kind::Global, Kind::Func))
        {
            return Err(format!(
                "`{spelled}` is a function in one of {} and {} and a variable in the other",
                units[best.unit].name, units[item.unit].name
            ));
        }
        let (now, then) = (item.rank(), best.rank());
        if now == 3 && then == 3 {
            return Err(format!(
                "`{spelled}` is defined in both {} and {}",
                units[best.unit].name, units[item.unit].name
            ));
        }
        if now > then || (now == 2 && then == 2 && item.size > best.size) {
            best = item;
        }
    }
    Ok(best)
}

/// What the kept item says once the others are taken into account: the linkage of a name only
/// declared, which is weak only when every unit's reference is, the visibility, which is the most
/// restricted any unit gave, and the alignment, which is the largest any common definition asked.
fn mended(winner: Item, list: &[Item]) -> (Linkage, Visibility, u32) {
    let linkage = if list.iter().all(|item| item.linkage == Linkage::Weak) {
        Linkage::Weak
    } else {
        Linkage::External
    };
    let visibility = if list.iter().any(|item| item.visibility == Visibility::Hidden) {
        Visibility::Hidden
    } else if list.iter().any(|item| item.visibility == Visibility::Protected) {
        Visibility::Protected
    } else {
        Visibility::Default
    };
    let align = if winner.linkage == Linkage::Common {
        list.iter().filter(|item| item.linkage == Linkage::Common).map(|item| item.align).max()
    } else {
        None
    };
    (linkage, visibility, align.unwrap_or(0))
}

/// A name for unit `unit`'s `name` that nothing has, which says where it came from.
fn fresh(name: Symbol, unit: usize, taken: &mut Set<Symbol>, names: &mut Interner) -> Symbol {
    let base = format!("{}.lto.{unit}", names.resolve(name));
    let mut spelling = base.clone();
    let mut again = 0;
    loop {
        let symbol = names.intern(&spelling);
        if taken.insert(symbol) {
            return symbol;
        }
        again += 1;
        spelling = format!("{base}.{again}");
    }
}

#[cfg(test)]
mod tests {
    use super::{Unit, join};
    use rucc_base::Interner;
    use rucc_ir::{Linkage, Module, SymbolRef, Visibility, print};
    use rucc_target::Isa;

    fn text(name: &str, body: &str) -> String {
        format!(
            "; ModuleID = '{name}'\n; format 0\ntarget triple = \"x86_64-unknown-linux-gnu\"\n\
             target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"\n\n{body}"
        )
    }

    /// A unit with a `static`, a common variable, a function that calls one the other unit
    /// defines, and a function nobody may define.
    const A: &str = "\
global @count : i32 = 0, align 4, linkage(internal)
global @shared : bytes 4 = { zero 4 }, align 4, linkage(common)

func @get() -> i32, linkage(external) {
block0:
    %0 = global_addr @count
    %1 = load.i32 %0, align 4
    %2 = call @put(%1) : (i32) -> i32
    return %2
}

func @put(i32) -> i32, linkage(external);

func @maybe() -> i32, linkage(weak);
";

    /// The other unit, with a `static` of the same name, a larger common of the same name, the
    /// function the first one calls, hidden, and a strong reference to the weak one.
    const B: &str = "\
global @count : i32 = 7, align 4, linkage(internal)
global @shared : bytes 8 = { zero 8 }, align 8, linkage(common)

func @put(i32) -> i32, linkage(external), visibility(hidden) {
block0(%0: i32):
    %1 = global_addr @count
    store %0 -> %1, align 4
    return %0
}

func @maybe() -> i32, linkage(external);
";

    fn joined(units: &[(&str, &str, Isa)], isa: Isa) -> Result<(Module, Interner), String> {
        let texts: Vec<String> = units.iter().map(|&(name, body, _)| text(name, body)).collect();
        let units: Vec<Unit<'_>> = units
            .iter()
            .zip(&texts)
            .map(|(&(name, _, isa), module)| Unit { name, module, isa })
            .collect();
        let mut names = Interner::new();
        join(&units, isa, &mut names).map(|module| (module, names))
    }

    fn lookup(module: &Module, names: &mut Interner, name: &str) -> Option<SymbolRef> {
        module.lookup(names.intern(name))
    }

    #[test]
    fn each_unit_keeps_its_own_static_and_the_definition_is_kept_over_the_declaration() {
        let base = Isa::baseline();
        let (module, mut names) = joined(&[("a.o", A, base), ("b.o", B, base)], base).unwrap();
        let printed = print(&module, &names);

        // The first unit keeps the name and the second's is renamed, everywhere it is used.
        let Some(SymbolRef::Global(first)) = lookup(&module, &mut names, "count") else { panic!() };
        let Some(SymbolRef::Global(second)) = lookup(&module, &mut names, "count.lto.1") else {
            panic!("{printed}")
        };
        assert_eq!(module[first].linkage, Linkage::Internal);
        assert_eq!(module[second].linkage, Linkage::Internal);
        assert!(printed.contains("%1 = global_addr @count.lto.1\n"), "{printed}");
        assert!(printed.contains("%0 = global_addr @count\n"), "{printed}");

        // One `put`, the definition, hidden because the unit that defined it said so.
        let Some(SymbolRef::Func(put)) = lookup(&module, &mut names, "put") else { panic!() };
        assert!(!module[put].is_declaration());
        assert_eq!(module[put].visibility, Visibility::Hidden);
        assert_eq!(module.counts().funcs, 3, "{printed}");

        // The larger common, with the larger alignment.
        let Some(SymbolRef::Global(shared)) = lookup(&module, &mut names, "shared") else {
            panic!()
        };
        assert_eq!((module[shared].size, module[shared].align), (8, 8));

        // A name one unit refers to strongly is not weak because another refers to it weakly.
        let Some(SymbolRef::Func(maybe)) = lookup(&module, &mut names, "maybe") else { panic!() };
        assert_eq!(module[maybe].linkage, Linkage::External);
    }

    #[test]
    fn a_name_defined_twice_is_left_for_the_linker_to_report() {
        let base = Isa::baseline();
        let error = joined(&[("a.o", A, base), ("c.o", A, base)], base).unwrap_err();
        assert_eq!(error, "`get` is defined in both a.o and c.o");
    }

    #[test]
    fn a_weak_definition_gives_way_to_a_strong_one_in_either_order() {
        let weak = "func @f() -> i32, linkage(weak) {\nblock0:\n    %0 = iconst.i32 1\n    \
                    return %0\n}\n";
        let strong = "func @f() -> i32, linkage(external) {\nblock0:\n    %0 = iconst.i32 2\n    \
                      return %0\n}\n";
        let base = Isa::baseline();
        for units in [
            [("w.o", weak, base), ("s.o", strong, base)],
            [("s.o", strong, base), ("w.o", weak, base)],
        ] {
            let (module, names) = joined(&units, base).unwrap();
            let printed = print(&module, &names);
            assert!(printed.contains("iconst.i32 2"), "{printed}");
            assert!(!printed.contains("iconst.i32 1"), "{printed}");
        }
    }

    #[test]
    fn a_named_block_is_renamed_in_the_unit_that_comes_second() {
        let body = |name: &str| {
            format!(
                "func @{name}(ptr), linkage(external) {{\nblock0(%0: ptr):\n    \
                 %1 = block_addr block1\n    indirect_br %1, block1\n\nblock1:\n    return\n\n\
                 labels:\n    block1 = @.Llbl.0\n}}\n"
            )
        };
        let (f, g) = (body("f"), body("g"));
        let base = Isa::baseline();
        let (module, names) =
            joined(&[("f.o", f.as_str(), base), ("g.o", g.as_str(), base)], base).unwrap();
        let printed = print(&module, &names);
        assert_eq!(printed.matches("block1 = @.Llbl.0\n").count(), 1, "{printed}");
        assert_eq!(printed.matches("block1 = @.Llbl.0.lto.1\n").count(), 1, "{printed}");
    }

    #[test]
    fn a_function_keeps_what_its_unit_was_built_for_when_the_link_is_built_for_more() {
        let base = Isa::baseline();
        let more = Isa::level("x86-64-v2").expect("a level");
        let (module, mut names) = joined(&[("a.o", A, base), ("b.o", B, more)], more).unwrap();
        let Some(SymbolRef::Func(get)) = lookup(&module, &mut names, "get") else { panic!() };
        let Some(SymbolRef::Func(put)) = lookup(&module, &mut names, "put") else { panic!() };
        assert_eq!(module[get].target, Some(base));
        assert_eq!(module[put].target, None);
    }

    #[test]
    fn a_unit_with_an_asm_at_file_scope_is_joined_only_when_nothing_in_it_is_renamed() {
        let base = Isa::baseline();
        let b = format!("module asm \"call count\"\n\n{B}");
        let error = joined(&[("a.o", A, base), ("b.o", b.as_str(), base)], base).unwrap_err();
        assert_eq!(error, "b.o: an `asm` at file scope may name a local the join renames");

        // First, its `count` keeps its name and the other unit's is renamed. The `asm` of a unit
        // with no clash is kept too, after it.
        let units =
            [("b.o", b.as_str(), base), ("a.o", A, base), ("c.o", "module asm \"nop\"\n", base)];
        let (module, names) = joined(&units, base).unwrap();
        assert_eq!(module.file_asms(), ["call count", "nop"], "{}", print(&module, &names));
    }

    #[test]
    fn a_unit_that_does_not_read_is_named() {
        let base = Isa::baseline();
        let error = joined(&[("a.o", A, base), ("bad.o", "func @", base)], base).unwrap_err();
        assert!(error.starts_with("bad.o: line "), "{error}");
    }
}
