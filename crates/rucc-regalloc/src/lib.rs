//! Both register allocators and the allocation checker.
//!
//! Design: `spec/10-backend.md`. Layer rank 10, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! Liveness is here, which is the question both allocators ask first: [`order`] lays a function
//! out in the line the encoder will emit it in, and [`live`] says where in that line each value
//! is wanted. So is [`moves`], which puts the moves an edge turns into in an order they can be
//! made in one at a time. The single pass allocator's decision is in [`assign`]: where every value
//! of a function goes, in one linear scan, which is what `-O0` asks for. The rewrite that makes
//! that decision true in the function is in [`rewrite`], and [`run`] is the two of them together,
//! which is the whole of the `-O0` allocator. The allocation checker lands next and the
//! backtracking allocator is M4.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-regalloc/0.3.3")]

pub mod assign;
pub mod live;
pub mod moves;
pub mod order;
pub mod rewrite;

/// What allocating a function produced.
///
/// The moves are handed back rather than written into the function because a move is an
/// instruction and an instruction belongs to a target, which `spec/10-backend.md` section 10.8
/// says this crate holds nothing of. The consumer turns each one into whatever its target moves a
/// register with.
#[derive(Debug, Clone)]
pub struct Allocation {
    /// Where every value of the function went, which is what the frame layout reads.
    pub assignment: assign::Assignment,
    /// The moves the places do not already make true, in the order they have to be made in.
    pub edits: Vec<rewrite::Edit>,
}

/// Allocates registers for a function the way `-O0` asks for, rewriting it as it goes.
///
/// This is the shape `spec/10-backend.md` section 10.4 gives an allocator: a function and the
/// registers it may use in, an assignment and the moves that make it true out. The backtracking
/// allocator will answer the same question the same way.
///
/// # Panics
///
/// Panics on a function the caller was told not to hand it, which is one with a critical edge,
/// one whose entry block has parameters, or one wanting more scratch registers at an instruction
/// than the environment holds back. See [`rewrite::rewrite`].
pub fn run(func: &mut rucc_mir::Func, env: &assign::Env) -> Allocation {
    let order = order::Order::of(func);
    let live = live::Live::of(func, &order);
    let assignment = assign::assign(func, &order, &live, env);
    let edits = rewrite::rewrite(func, &assignment, env);
    Allocation { assignment, edits }
}

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
