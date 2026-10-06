//! Each `switch` split into clusters and a search over them, by the bounds that suit wasm.
//!
//! The clustering is the one the native back ends run, in `rucc_legalize::switch`, and section 7.4
//! of the WebAssembly notes says what changes on wasm. A dense stretch is left as a `switch` from
//! zero, which the selector writes as one `br_table`. The selector writes each `switch` that is
//! left this way as a table, so the bounds here are the only ones that decide.
//!
//! Two bounds are not the native ones. A `br_table` sends an index outside its cells to its
//! default, so a table that is the last test before the default gets no range check of its own.
//! And how many clusters a table needs is measured on Wasmtime, where a cell is one byte and the
//! engine writes the jump. See `WASM_JUMP_TABLE_MIN_TARGETS` in `rucc-cost`. The rest, which are
//! how dense a table must be and how long a walk is before a search splits it, are the native
//! ones, since the same measurements agree with them.

use rucc_cost::Goal;
use rucc_cost::heuristics::{WASM_JUMP_TABLE_MIN_TARGETS, WASM_JUMP_TABLE_MIN_TARGETS_FOR_SIZE};
use rucc_ir::Module;
use rucc_legalize::switch::{self, Rules};

/// Split each `switch` in `module` into the tests and tables that the selector writes, for code
/// that is fast or small as `goal` says. `tables` is false under `-fno-jump-tables`, and then no
/// `switch` becomes a table. The word is 32 bits, which is the type of a `br_table` index.
pub fn switches(module: &mut Module, goal: Goal, tables: bool) {
    let least = match goal {
        Goal::Speed => WASM_JUMP_TABLE_MIN_TARGETS,
        Goal::Size => WASM_JUMP_TABLE_MIN_TARGETS_FOR_SIZE,
    };
    let rules = Rules { least, checked: true, ..Rules::native(goal) };
    for id in module.funcs().collect::<Vec<_>>() {
        if !module[id].is_declaration() {
            let _ = switch::lowered_by(&mut module[id], rules, None, tables, 32);
        }
    }
}
