//! Which functions only ever run when something unlikely happened.
//!
//! A function written `cold` is one, and so is a `static` function whose every call is in one of
//! those, which is what gcc's `ipa_propagate_frequency` works out without a profile. gcc puts both
//! in `.text.unlikely`. In the kernel the second kind is what an `__init` or `__exit` function
//! leaves behind when it calls a `static inline` helper gcc did not copy into it, such as
//! `__nr_to_section` in mm/sparse.c, or a `static` function only the module's exit calls, such as
//! `snd_timer_free_all` in sound/core/timer.c.
//!
//! Asked of the module as it is handed to the back end, so a call that was inlined is not a call
//! any more and does not count. A function anything reaches other than by a direct call in this
//! unit is never one of the second kind, since this unit cannot see all of its callers.

use rucc_base::hash::Set;
use rucc_ir::{AttrSet, FuncId, Linkage, Module, Pic};

use crate::callgraph::CallGraph;

/// Every function in the module that only runs when something unlikely happened.
#[must_use]
pub fn functions(module: &Module) -> Set<FuncId> {
    let mut unlikely: Set<FuncId> =
        module.funcs().filter(|&id| module[id].attrs.set.contains(AttrSet::COLD)).collect();
    let graph = CallGraph::of(module, Pic::Executable);
    // Who calls each node, each caller once, other than the node itself, since a function calling
    // itself says nothing about how often anything else calls it.
    let mut callers = vec![Vec::new(); graph.len()];
    for node in graph.nodes() {
        for &callee in graph.calls(node) {
            if callee != node {
                callers[callee.index()].push(node);
            }
        }
    }
    let local: Vec<(FuncId, usize)> = graph
        .nodes()
        .filter_map(|node| {
            let id = graph.func(node)?;
            let func = &module[id];
            let local = !func.is_declaration()
                && func.linkage == Linkage::Internal
                && !graph.address_taken(node)
                && !callers[node.index()].is_empty();
            local.then_some((id, node.index()))
        })
        .collect();
    // Round until nothing moves. Only ever adding, so a function stays out unless every caller is
    // in, which is gcc's answer for two functions that only call each other too.
    loop {
        let mut moved = false;
        for &(id, at) in &local {
            if unlikely.contains(&id) {
                continue;
            }
            let all = callers[at]
                .iter()
                .all(|&caller| graph.func(caller).is_some_and(|f| unlikely.contains(&f)));
            if all {
                unlikely.insert(id);
                moved = true;
            }
        }
        if !moved {
            return unlikely;
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::*;

    fn names(source: &str) -> Vec<String> {
        let mut names = Interner::new();
        let text = format!(
            "; ModuleID = 't.c'\n; format 0\ntarget triple = \"x86_64-unknown-linux-gnu\"\n\
             target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"\n{source}"
        );
        let module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let mut out: Vec<String> = functions(&module)
            .into_iter()
            .map(|id| names.resolve(module[id].name).to_owned())
            .collect();
        out.sort();
        out
    }

    /// A `static` function only cold functions call is unlikely, one that a function that is not
    /// cold calls as well is not, and neither is one whose address is taken.
    #[test]
    fn a_static_function_only_cold_functions_call_is_unlikely() {
        let out = names(
            r#"
func @helper(i32) -> i32, linkage(internal) {
block0(%0: i32):
    return %0
}

func @deeper(i32) -> i32, linkage(internal) {
block0(%0: i32):
    %1 = call @helper(%0) : (i32) -> i32
    return %1
}

func @shared(i32) -> i32, linkage(internal) {
block0(%0: i32):
    return %0
}

func @taken(i32) -> i32, linkage(internal) {
block0(%0: i32):
    return %0
}

func @setup(i32) -> i32, linkage(external), attrs(cold) {
block0(%0: i32):
    %1 = call @deeper(%0) : (i32) -> i32
    %2 = call @shared(%1) : (i32) -> i32
    %3 = call @taken(%2) : (i32) -> i32
    return %3
}

func @run(i32) -> ptr, linkage(external) {
block0(%0: i32):
    %1 = call @shared(%0) : (i32) -> i32
    %2 = global_addr @taken
    return %2
}
"#,
        );
        assert_eq!(out, ["deeper", "helper", "setup"]);
    }
}
