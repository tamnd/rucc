//! What both interprocedural transformations need: which functions are theirs to change.
//!
//! Design: `spec/optimizer/34-ipa.md` section 34.6. The two transformations M4 builds, the constant
//! propagation in [`crate::ipcp`] and the parameter removal in [`crate::ipasra`], ask the same
//! question before they touch anything, and the spec asks it of both in the same words: any
//! function whose address is taken or which is externally visible cannot be changed at all.
//!
//! It is one gate here rather than one in each of them because a gate two passes each carry a copy
//! of is a gate two passes can come to disagree about. The disagreement that would follow is a
//! function one of them rewrote the body of and the other rewrote the calls to, with neither of
//! them wrong on its own.
//!
//! Nothing in here transforms anything. It reads the module and the call graph and says what is
//! reachable from where, which is what makes it safe for a pass to ask again after it has changed
//! something.

use std::collections::{HashMap, HashSet};

use rucc_ir::{Extra, Func, FuncId, Inst, Linkage, Module, Opcode, Value};

use crate::CallGraph;

/// The functions this unit can see every call to, with the parameters lined up.
///
/// Five things, and the first three are one thing said three ways. Internal linkage means no other
/// object can name it. No address taken means nothing in this one can reach it except by naming it.
/// A body this unit may read is [`CallGraph::trusted_body`], which is section 34.1's gate, and
/// without it there is nothing to put a constant into. Then the signature has to have a fixed
/// number of parameters, and the entry block has to have one value per parameter, which is what
/// makes the position of an argument at a call the position of a parameter in the body.
pub fn closed(module: &Module, graph: &CallGraph) -> Vec<FuncId> {
    let mut closed = Vec::new();
    for node in graph.nodes() {
        if graph.address_taken(node) {
            continue;
        }
        let Some(id) = graph.trusted_body(node) else { continue };
        let func = &module[id];
        if func.linkage != Linkage::Internal || func.signature().variadic {
            continue;
        }
        let Some(entry) = func.entry() else { continue };
        if func[entry].params.len() != func.signature().params.len() {
            continue;
        }
        // A body that saves the registers it was called with reads its arguments where the
        // convention put them rather than as parameters, so a parameter taken out or a constant
        // put in its place would leave it reading something the caller no longer passes.
        let saves = func
            .blocks()
            .any(|block| func.insts(block).any(|inst| func[inst].opcode == Opcode::ApplyArgs));
        if saves {
            continue;
        }
        closed.push(id);
    }
    // Module order, for the reason the return above gives.
    closed.sort_unstable_by_key(|id| id.raw());
    closed
}

/// The components of the call graph with callers before callees, each holding only closed nodes.
///
/// [`CallGraph::components`] is callees first, so this is that read backwards. A component with no
/// closed function in it is left out rather than walked over, since the round inside it would
/// compute nothing.
pub fn order(graph: &CallGraph, closed: &[FuncId]) -> Vec<Vec<FuncId>> {
    let inside: HashSet<FuncId> = closed.iter().copied().collect();
    let mut order = Vec::new();
    for part in graph.components().iter().rev() {
        let part: Vec<FuncId> = part
            .iter()
            .filter_map(|&node| graph.trusted_body(node))
            .filter(|id| inside.contains(id))
            .collect();
        if !part.is_empty() {
            order.push(part);
        }
    }
    order
}

/// Every direct call in the module to one of the closed functions, by the function called.
///
/// Every call, not only the ones from closed functions. A call from anywhere is a call, and what
/// the caller is only matters when the argument is the caller's own parameter, which is the one
/// place below that asks.
///
/// A call whose argument count does not match what the callee takes is a prototype disagreeing with
/// a definition, which a translation unit may contain. The positions would not line up, so the call
/// is not read and the callee is struck out instead of being read from the rest of its calls, since
/// what that one passes is exactly what is not known.
pub fn sites(module: &Module, closed: &[FuncId]) -> HashMap<FuncId, Sites> {
    let mut where_defined: HashMap<_, FuncId> = HashMap::new();
    for &id in closed {
        where_defined.insert(module[id].name, id);
    }
    let mut sites: HashMap<FuncId, Sites> = HashMap::new();
    for id in module.funcs() {
        let func = &module[id];
        if func.is_declaration() {
            continue;
        }
        for block in func.blocks() {
            for inst in func.insts(block) {
                if !matches!(func[inst].opcode, Opcode::Call | Opcode::TailCall) {
                    continue;
                }
                let Extra::Call(at) = func[inst].extra else { continue };
                let Some(callee) = func[at].callee else { continue };
                let Some(&target) = where_defined.get(&callee) else { continue };
                let entry = sites.entry(target).or_default();
                if module[target].signature().params.len() != func[func[inst].args].len() {
                    entry.ragged = true;
                    continue;
                }
                entry.calls.push((id, inst));
            }
        }
    }
    sites
}

/// Where one function is called from.
#[derive(Debug, Default)]
pub struct Sites {
    /// The caller and the instruction, for every call whose arguments line up.
    pub calls: Vec<(FuncId, Inst)>,
    /// Whether some call passed a number of arguments the function does not take.
    ///
    /// One of those and nothing is claimed about any parameter, because the call is real and what
    /// it passed is what cannot be read.
    pub ragged: bool,
}

/// Every value the body reads, as an operand or as an argument on an edge.
pub fn operands(func: &Func) -> HashSet<Value> {
    let mut read = HashSet::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            read.extend(func[func[inst].args].iter().copied());
            for edge in func.successors(inst) {
                read.extend(func[edge.args].iter().copied());
            }
        }
    }
    read
}
