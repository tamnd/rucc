//! Functions that are nothing but a shape, for the tests of the analyses that read one.
//!
//! An analysis of the control flow graph does not care what is in the blocks, so the tests of
//! one should not have to say. These build a function out of an edge list and put the smallest
//! terminator that produces those edges in each block.
//!
//! This is compiled into the test build of the crate and also read directly by
//! `tests/dominators.rs` through a `#[path]` module, so the six graphs the design document
//! names and the unit tests here are written against one builder rather than two that drift.

use rucc_base::Interner;
use rucc_ir::{Block, Builder, Func, Signature, Type};

/// A function whose blocks branch the way the list says, and do nothing else.
///
/// Entry is block 0, and entry number `n` holds the block numbers block `n` branches to. No
/// successors is a `return`, one is a `jump`, two are the arms of a `br_if`, and more are a
/// `switch` whose first target is the default. The values the terminators need are constants,
/// because no test here reads them.
///
/// # Panics
///
/// Panics if the list names a block that is not in it.
pub(crate) fn graph(edges: &[&[usize]]) -> Func {
    let mut names = Interner::new();
    let mut func = Func::new(names.intern("f"), Signature::new());
    let blocks: Vec<Block> = edges.iter().map(|_| func.create_block()).collect();
    for (index, targets) in edges.iter().enumerate() {
        assert!(targets.iter().all(|&t| t < blocks.len()), "block {index} branches to nowhere");
        let mut build = Builder::new(&mut func, blocks[index]);
        match targets {
            [] => {
                build.ret(&[]);
            }
            [only] => {
                build.jump(blocks[*only], &[]);
            }
            [taken, not_taken] => {
                let cond = build.iconst(Type::int(1), 1);
                build.br_if(cond, blocks[*taken], &[], blocks[*not_taken], &[]);
            }
            [default, cases @ ..] => {
                let value = build.iconst(Type::int(32), 0);
                let cases: Vec<(i128, Block)> = cases
                    .iter()
                    .enumerate()
                    .map(|(case, &target)| (case as i128, blocks[target]))
                    .collect();
                build.switch(value, blocks[*default], &cases);
            }
        }
    }
    func
}

/// A function where the only way into a block is an `indirect_br` through an address that was
/// taken in a different block.
///
/// Block 0 takes block 2's address and jumps to block 1, block 1 branches to the address, and
/// block 2 returns. The graph has one edge into block 2 and it comes from block 1. The verifier
/// counts block 0 as a predecessor as well, on purpose and soundly for what it is checking, and
/// an analysis that copied that would report a live block as having a predecessor that never
/// branches anywhere.
pub(crate) fn computed_goto() -> Func {
    let mut names = Interner::new();
    let mut func = Func::new(names.intern("f"), Signature::new());
    let entry = func.create_block();
    let middle = func.create_block();
    let target = func.create_block();

    let mut build = Builder::new(&mut func, entry);
    let addr = build.block_addr(target);
    build.jump(middle, &[]);

    let mut build = Builder::new(&mut func, middle);
    build.indirect_br(addr, &[target]);

    let mut build = Builder::new(&mut func, target);
    build.ret(&[]);

    func
}
