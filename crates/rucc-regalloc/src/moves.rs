//! Putting a set of moves that happen at once into an order they can happen in one at a time.
//!
//! Design: `spec/10-backend.md` section 10.4.
//!
//! The parameters of a block arrive from every edge into it, and after allocation each of them is
//! a place and each argument is a place, so an edge becomes a set of moves. They all happen at
//! once: every argument is read as it was at the end of the predecessor, and nothing an edge
//! writes is visible to anything else the edge writes. A machine does not have that instruction.
//! It has one move, and the moves have to go in an order.
//!
//! Most of the time any order will do, but not always. Two parameters that swap two values are
//! two moves that each destroy what the other wants to read, and no order of the two is right.
//! The way out is a third place to keep one of the values in, which is the scratch, and the
//! algorithm below is the one that finds out when one is needed and writes as few extra moves as
//! it can. `spec/10-backend.md` says this is a small algorithm that is wrong in a startling number
//! of compilers, which is why it is written here on its own and tested on its own rather than
//! being a loop inside the allocator.
//!
//! Nothing here knows what a place is. A place is a register after allocation, or a stack slot
//! for a value that was spilled, and the algorithm is the same either way, so the caller says what
//! its places are and gets the same kind of thing back.

/// One move: what is written, and what is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Move<T> {
    /// The place written.
    pub to: T,
    /// The place read.
    pub from: T,
}

impl<T> Move<T> {
    /// A move from one place to another.
    pub const fn new(to: T, from: T) -> Self {
        Self { to, from }
    }
}

/// The moves in an order they can be made in, one at a time.
///
/// The result writes exactly the places the input writes, and every one of them ends up holding
/// what the input said it should, except that the scratch may hold anything afterwards. A move
/// from a place to itself is not in the result, because it is nothing to do.
///
/// The scratch is written only when a set of moves is a cycle, which is the case no order can
/// answer on its own. Nothing else in the moves may name it, since it is the one place the
/// algorithm is free to destroy.
///
/// # Panics
///
/// Panics if two moves write the same place. That is not a set of parallel moves, it is a
/// question about which of two values a place ends up holding, and the caller has to answer it
/// before asking for an order.
#[must_use]
pub fn sequence<T: Copy + PartialEq>(moves: &[Move<T>], scratch: T) -> Vec<Move<T>> {
    let mut pending: Vec<Move<T>> =
        moves.iter().copied().filter(|one| one.to != one.from).collect();
    for (index, one) in pending.iter().enumerate() {
        assert!(
            !pending[..index].iter().any(|earlier| earlier.to == one.to),
            "two parallel moves write the same place"
        );
    }

    let mut order = Vec::with_capacity(pending.len());
    while !pending.is_empty() {
        // A move whose destination nothing else still has to read is one that can go now, and
        // taking those first is what keeps a chain of moves a chain of moves.
        let ready = pending.iter().position(|one| {
            !pending.iter().any(|other| other.from == one.to && other.to != one.to)
        });
        match ready {
            Some(index) => order.push(pending.remove(index)),
            None => break_cycle(&mut pending, &mut order, scratch),
        }
    }
    order
}

/// Puts one value of a cycle somewhere safe, which leaves the rest of it an ordinary chain.
fn break_cycle<T: Copy + PartialEq>(pending: &mut [Move<T>], order: &mut Vec<Move<T>>, scratch: T) {
    // Every move left is in a cycle, so any of them will do. Reading the first one's source into
    // the scratch means nothing wants that source any more, so the move that writes it is free to
    // go, and the move that wanted it reads the scratch instead.
    let source = pending[0].from;
    order.push(Move::new(scratch, source));
    for one in pending.iter_mut().filter(|one| one.from == source) {
        one.from = scratch;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Moves between places named by a letter, which is what a test can read.
    fn moves(pairs: &[(char, char)]) -> Vec<Move<char>> {
        pairs.iter().map(|&(to, from)| Move::new(to, from)).collect()
    }

    /// What the places hold after the moves are made in that order, starting from each place
    /// holding its own name.
    fn run(order: &[Move<char>]) -> Vec<(char, char)> {
        let mut held: Vec<(char, char)> = ('a'..='z').map(|place| (place, place)).collect();
        for one in order {
            let value = held.iter().find(|&&(place, _)| place == one.from).expect("a place").1;
            held.iter_mut().find(|(place, _)| *place == one.to).expect("a place").1 = value;
        }
        held
    }

    /// Whether an order leaves every place holding what the parallel moves said it should.
    fn correct(parallel: &[Move<char>], order: &[Move<char>]) -> bool {
        let held = run(order);
        parallel.iter().all(|one| {
            held.iter().find(|&&(place, _)| place == one.to).expect("a place").1 == one.from
        })
    }

    #[test]
    fn moves_that_get_in_nobody_s_way_are_made_in_any_order() {
        let parallel = moves(&[('a', 'b'), ('c', 'd')]);
        let order = sequence(&parallel, 'z');
        assert_eq!(order.len(), 2);
        assert!(correct(&parallel, &order));
    }

    #[test]
    fn a_chain_is_made_from_the_end_of_it() {
        // `a` gets what is in `b` and `b` gets what is in `c`, so `b` has to be read before it is
        // written and no scratch is needed to see that.
        let parallel = moves(&[('b', 'c'), ('a', 'b')]);
        let order = sequence(&parallel, 'z');
        assert_eq!(order, moves(&[('a', 'b'), ('b', 'c')]));
        assert!(correct(&parallel, &order));
    }

    #[test]
    fn two_values_that_swap_need_somewhere_to_put_one_of_them() {
        let parallel = moves(&[('a', 'b'), ('b', 'a')]);
        let order = sequence(&parallel, 'z');
        assert!(correct(&parallel, &order));
        assert_eq!(order.len(), 3);
        assert!(order.iter().any(|one| one.to == 'z'));
    }

    #[test]
    fn a_longer_cycle_costs_the_same_one_extra_move() {
        // Three values going round: `a` takes `b`'s, `b` takes `c`'s and `c` takes `a`'s.
        let parallel = moves(&[('a', 'b'), ('b', 'c'), ('c', 'a')]);
        let order = sequence(&parallel, 'z');
        assert!(correct(&parallel, &order));
        assert_eq!(order.len(), 4);
    }

    #[test]
    fn two_cycles_are_broken_one_at_a_time() {
        let parallel = moves(&[('a', 'b'), ('b', 'a'), ('c', 'd'), ('d', 'c')]);
        let order = sequence(&parallel, 'z');
        assert!(correct(&parallel, &order));
        // The scratch is reused, because it is free again as soon as the first cycle is closed.
        assert_eq!(order.len(), 6);
    }

    #[test]
    fn a_value_wanted_in_two_places_is_read_twice() {
        let parallel = moves(&[('a', 'c'), ('b', 'c')]);
        let order = sequence(&parallel, 'z');
        assert!(correct(&parallel, &order));
        assert_eq!(order.len(), 2);
        assert!(!order.iter().any(|one| one.to == 'z'));
    }

    #[test]
    fn a_cycle_with_a_tail_hanging_off_it_is_still_one_extra_move() {
        // `d` also wants what is in `a`, which is not part of the cycle and has to be read before
        // the cycle overwrites it.
        let parallel = moves(&[('a', 'b'), ('b', 'a'), ('d', 'a')]);
        let order = sequence(&parallel, 'z');
        assert!(correct(&parallel, &order));
        assert_eq!(order.len(), 4);
    }

    #[test]
    fn a_move_from_a_place_to_itself_is_nothing_to_do() {
        let order = sequence(&moves(&[('a', 'a'), ('b', 'c')]), 'z');
        assert_eq!(order, moves(&[('b', 'c')]));
    }

    #[test]
    fn nothing_to_move_is_nothing_to_do() {
        assert_eq!(sequence::<char>(&[], 'z'), []);
    }

    #[test]
    #[should_panic(expected = "two parallel moves write the same place")]
    fn two_moves_that_write_one_place_are_not_a_question_this_can_answer() {
        let _ = sequence(&moves(&[('a', 'b'), ('a', 'c')]), 'z');
    }
}
