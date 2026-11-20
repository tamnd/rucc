//! The order the functions are written into the object in, which is the order gcc writes them.
//!
//! At `-O0` gcc writes the bodies in the order the source wrote them. From `-O1` up it has
//! `-ftoplevel-reorder`, and `expand_all_functions` writes them in the reverse of what
//! `ipa_reverse_postorder` gives, which puts a callee ahead of its callers where it can. The order
//! is not something a program can see, but objtool can. It forgives the code of a weak function
//! another object replaced only when that code sits between two symbols, and a weak function
//! written first in its section, ahead of every symbol, is reported as an instruction nothing
//! reaches. Linux 6.12's `poking_init` in init/main.c is that, and gcc writes it well after the
//! `static` handlers `__setup` registers.
//!
//! A function written `__attribute__((no_reorder))` is the exception. gcc writes every one of
//! those first, in the order the source wrote them, and the rest after them in the order below.
//!
//! # The walk
//!
//! gcc keeps its symbols in a list it puts each new one at the front of, so walking it starts at
//! the one made last. A body is made when the source finishes writing it. A name only called is
//! made when the call is read, which is after every body, in the order `analyze_functions` reads
//! them: it starts from what another object can reach, oldest first, and goes on from each body to
//! what that body calls and names before it goes back to the next. A clone the optimizer made
//! comes after all of them.
//!
//! Two passes go down that list. The first starts only from functions another object can call and
//! nothing takes the address of. The second starts from everything left. From each start the walk
//! goes to the callers, the one read last first, and puts a function down once every caller it has
//! not been to is down. The functions come out in the reverse of the order they were put down.

use rucc_base::Symbol;
use rucc_base::hash::{Map, Set};
use rucc_ir::{AttrSet, Datum, Extra, FuncId, GlobalId, Linkage, Module, Opcode, SymbolRef};

/// Every function with a body, in the order gcc would write them, with `reorder` saying whether
/// `-ftoplevel-reorder` is on.
#[must_use]
pub fn order(module: &Module, reorder: bool) -> Vec<FuncId> {
    let written = module.in_written_order();
    if !reorder {
        return written;
    }
    // A module that says nothing of what the source wrote, which is one read back from text, is
    // taken to hold its bodies in the order they were written.
    let mut wrote = vec![module.written().is_empty(); module.funcs().count()];
    for &id in module.written() {
        wrote[id.index()] = true;
    }
    let (source, passes): (Vec<FuncId>, Vec<FuncId>) =
        written.iter().partition(|id| wrote[id.index()]);

    let mut nodes = Nodes::default();
    for &id in &source {
        nodes.make(module[id].name);
    }
    // What gcc reads first: the functions and the variables another object can reach or the
    // program said to keep, oldest on top. The variables are taken to come after the functions,
    // which is where a table of operations usually is.
    let mut roots: Vec<Item> = source
        .iter()
        .filter(|&&id| module[id].linkage != Linkage::Internal || used(module, id))
        .map(|&id| Item::Func(id))
        .collect();
    let globals: Vec<GlobalId> = module.globals().collect();
    roots.extend(
        globals
            .iter()
            .rev()
            .filter(|&&id| {
                let global = &module[id];
                global.init.is_some() && (global.linkage != Linkage::Internal || !global.droppable)
            })
            .map(|&id| Item::Global(id)),
    );
    let mut queued: Set<Item> = Set::default();
    let mut stack = Vec::new();
    for &root in roots.iter().rev() {
        if queued.insert(root) {
            stack.push(root);
        }
    }
    let mut read = vec![None; wrote.len()];
    let mut next = 0;
    while let Some(item) = stack.pop() {
        let names = named(module, item);
        for &(name, call) in &names {
            match module.lookup(name) {
                Some(SymbolRef::Func(_) | SymbolRef::Alias(_)) => nodes.make(name),
                None if call => nodes.make(name),
                _ => {}
            }
        }
        if let Item::Func(id) = item {
            read[id.index()] = Some(next);
            next += 1;
        }
        // The calls go on the stack last first, since gcc puts each new edge at the front of the
        // list it walks, and what is named other than by a call in the order it was named.
        let calls = names.iter().filter(|(_, call)| *call).rev();
        let others = names.iter().filter(|(_, call)| !*call);
        for &(name, _) in calls.chain(others) {
            let Some(to) = defined(module, name) else { continue };
            if queued.insert(to) {
                stack.push(to);
            }
        }
    }
    for &id in &passes {
        nodes.make(module[id].name);
    }

    // The calls left once the optimizer is done, by callee, the caller read last first.
    let count = nodes.made.len();
    let mut callers: Vec<Vec<FuncId>> = vec![Vec::new(); count];
    let mut taken = vec![false; count];
    for &id in &written {
        for (name, call) in named(module, Item::Func(id)) {
            let Some(to) = nodes.at(module, name) else { continue };
            if call {
                if !callers[to].contains(&id) {
                    callers[to].push(id);
                }
            } else {
                taken[to] = true;
            }
        }
    }
    for id in module.globals() {
        for (name, _) in named(module, Item::Global(id)) {
            if let Some(to) = nodes.at(module, name) {
                taken[to] = true;
            }
        }
    }
    for list in &mut callers {
        list.sort_by_key(|id| std::cmp::Reverse(read[id.index()]));
    }
    let func = |node: usize| match module.lookup(nodes.made[node]) {
        Some(SymbolRef::Func(id)) if !module[id].is_declaration() => Some(id),
        _ => None,
    };
    let first = |node: usize| {
        func(node).is_some_and(|id| module[id].linkage != Linkage::Internal || used(module, id))
            && !taken[node]
    };
    let mut down = Vec::with_capacity(count);
    let mut seen = vec![false; count];
    for pass in [true, false] {
        for start in (0..count).rev() {
            if seen[start] || (pass && !first(start)) {
                continue;
            }
            seen[start] = true;
            let mut stack = vec![(start, 0)];
            while let Some(&mut (node, ref mut at)) = stack.last_mut() {
                let up = callers[node].get(*at).and_then(|&id| nodes.at(module, module[id].name));
                *at += 1;
                match up {
                    Some(up) if !seen[up] => {
                        seen[up] = true;
                        stack.push((up, 0));
                    }
                    Some(_) => {}
                    None if *at <= callers[node].len() => {}
                    None => {
                        down.push(node);
                        stack.pop();
                    }
                }
            }
        }
    }
    // A function written `no_reorder` is not gcc's to move. `output_in_order` writes those first,
    // in the order the source wrote them, and the walk above places the rest without them.
    let fixed = |id: &FuncId| module[*id].attrs.set.contains(AttrSet::NO_REORDER);
    let walked = down.iter().rev().filter_map(|&node| func(node)).filter(|id| !fixed(id));
    written.iter().copied().filter(fixed).chain(walked).collect()
}

/// A body or an image, which is what gcc reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Item {
    Func(FuncId),
    Global(GlobalId),
}

/// The names gcc has made a node for, in the order it made them.
#[derive(Default)]
struct Nodes {
    made: Vec<Symbol>,
    index: Map<Symbol, usize>,
}

impl Nodes {
    fn make(&mut self, name: Symbol) {
        if !self.index.contains_key(&name) {
            self.index.insert(name, self.made.len());
            self.made.push(name);
        }
    }

    /// The node a name is, with a second name taken to be the function it names.
    fn at(&self, module: &Module, name: Symbol) -> Option<usize> {
        let mut name = name;
        for _ in 0..8 {
            match module.lookup(name) {
                Some(SymbolRef::Alias(id)) => name = module[id].target,
                _ => break,
            }
        }
        self.index.get(&name).copied()
    }
}

/// Whether the program said to keep a function whatever refers to it.
fn used(module: &Module, id: FuncId) -> bool {
    module[id].attrs.set.contains(AttrSet::USED)
}

/// The body or image a name is, when the module has one.
fn defined(module: &Module, name: Symbol) -> Option<Item> {
    let mut name = name;
    for _ in 0..8 {
        match module.lookup(name)? {
            SymbolRef::Func(id) => return (!module[id].is_declaration()).then_some(Item::Func(id)),
            SymbolRef::Global(id) => return module[id].init.is_some().then_some(Item::Global(id)),
            SymbolRef::Alias(id) => name = module[id].target,
        }
    }
    None
}

/// What a body or an image names, in the order it names them, each with whether it is a call.
fn named(module: &Module, item: Item) -> Vec<(Symbol, bool)> {
    match item {
        Item::Func(id) => {
            let func = &module[id];
            let mut names = Vec::new();
            for block in func.blocks() {
                for inst in func.insts(block) {
                    let data = &func[inst];
                    match (data.opcode, data.extra) {
                        (Opcode::Call | Opcode::TailCall, Extra::Call(at)) => {
                            names.extend(func[at].callee.map(|name| (name, true)));
                        }
                        (Opcode::GlobalAddr, Extra::Symbol(name)) => names.push((name, false)),
                        _ => {}
                    }
                }
            }
            names
        }
        Item::Global(id) => {
            let init = module[id].init.map(|list| &module[list]).unwrap_or_default();
            init.iter()
                .filter_map(|datum| match *datum {
                    Datum::Addr(reloc) | Datum::Away(reloc) => Some((module[reloc].symbol, false)),
                    _ => None,
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rucc_base::Interner;

    const HEAD: &str = "; ModuleID = 't.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"

";

    /// The names of the functions in the order they come out, for a module whose functions are
    /// written as `name linkage callees...`, one to a line, in the order the source wrote them.
    fn order_of(funcs: &str, reorder: bool) -> String {
        let mut text = HEAD.to_owned();
        for line in funcs.lines() {
            let mut words = line.split_whitespace();
            let (Some(name), Some(linkage)) = (words.next(), words.next()) else { continue };
            if linkage == "declared" {
                text.push_str(&format!("func @{name}(), linkage(external);\n\n"));
                continue;
            }
            text.push_str(&format!("func @{name}(), linkage({linkage}) {{\nblock0:\n"));
            for callee in words {
                text.push_str(&format!("    call @{callee}() : ()\n"));
            }
            text.push_str("    return\n}\n\n");
        }
        let mut names = Interner::new();
        let module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        order(&module, reorder)
            .into_iter()
            .map(|id| names.resolve(module[id].name).to_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn a_callee_comes_out_ahead_of_its_callers_and_the_static_ones_ahead_of_the_rest() {
        // What gcc 12 writes at -O2 for these, with every function `noinline`.
        let funcs = "a external f
            b external f
            f external
            k internal g h
            k2 internal g h
            g internal
            h internal
            e external k k2";
        assert_eq!(order_of(funcs, true), "g h k k2 f a b e");
        assert_eq!(order_of(funcs, false), "a b f k k2 g h e");
    }

    #[test]
    fn a_name_only_declared_is_walked_from_first() {
        // gcc makes the node for `ext` when it reads the call, after every body, so the walk
        // starts there and reaches `s1` before it reaches `s2`.
        let funcs = "ext declared
            s1 internal ext
            s2 internal
            e external s1 s2";
        assert_eq!(order_of(funcs, true), "s2 s1 e");
    }
}
