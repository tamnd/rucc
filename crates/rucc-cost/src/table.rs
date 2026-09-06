//! The table a target fills in, and the machinery that makes filling it in completely mandatory.
//!
//! Section 40.13 names the failure this module is built around. A target's table is incomplete, a
//! field is left at zero, and a zero cost makes an operation free, so every heuristic that consults
//! it goes wrong in the same direction and none of them look broken. The section is explicit about
//! what a defence has to do: it "must check every field, not merely that the struct was
//! constructed".
//!
//! Rust will not do that on its own. A struct literal that names every field compiles, and so does
//! one that names half of them and ends in `..Default::default()`, and the second is how a table
//! ends up with a zero nobody chose. So [`CostTable`] has no `Default`, no public fields to
//! construct it through, and one way in: a builder that remembers which fields were set and
//! refuses to hand over a table while any are missing. The completeness check is the constructor,
//! which means it cannot be the test somebody forgot to write.
//!
//! # Two groups, per section 40.3
//!
//! GCC's `struct processor_costs` has about 107 fields split in two, and the comment at
//! `gcc/config/i386/i386.h:114` says why: the register allocator's costs for moving a value between
//! two places and the expression evaluator's costs for the same operations are different questions
//! with different answers. The fields below keep that split by name, `move_*` for the allocator and
//! everything else for the evaluator.
//!
//! This table is smaller than GCC's, and the parts that are missing are missing because rucc has no
//! pass that would read them yet: the vector and mask register costs, the gather and scatter
//! formulas, the `memcpy` and `memset` strategy tables, the cache and prefetch parameters, and the
//! alignment strings. Each of those belongs with the pass that needs it, and adding a field nobody
//! reads is adding a number nobody will check.

use crate::{Bytes, Cycles};

/// An integer width, in the four sizes the machine has instructions for.
///
/// Four rather than GCC's five. `mult_init[5]` and `divide[5]` on x86-64 index by mode up to
/// `TImode`, and rucc lowers 128-bit arithmetic to calls or to pairs of 64-bit operations rather
/// than costing it as one instruction, so a fifth entry would be a number no pass could use
/// honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Width {
    /// One byte.
    W8,
    /// Two bytes.
    W16,
    /// Four bytes.
    W32,
    /// Eight bytes.
    W64,
}

impl Width {
    /// Every width, in order, for a table to be written against.
    pub const ALL: [Self; 4] = [Self::W8, Self::W16, Self::W32, Self::W64];

    /// Where this width sits in a width-indexed field.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The width in bits, for a caller that has one and wants the other.
    #[must_use]
    pub const fn bits(self) -> u32 {
        match self {
            Self::W8 => 8,
            Self::W16 => 16,
            Self::W32 => 32,
            Self::W64 => 64,
        }
    }

    /// The width a number of bits names, or nothing for a width the machine has no name for.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            8 => Some(Self::W8),
            16 => Some(Self::W16),
            32 => Some(Self::W32),
            64 => Some(Self::W64),
            _ => None,
        }
    }
}

/// The shape of a memory address, which is what an addressing mode table is indexed by.
///
/// Section 40.9 wants these costed per target and wants the complexity counted per structural
/// feature, and [`AddrMode::complexity`] is that count. A mode the target does not have is
/// [`Cycles::INFINITE`] in the table rather than absent from it, so that a pass asking about a mode
/// gets an answer it can compare instead of an `Option` it has to unwrap into a policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AddrMode {
    /// `[base]`.
    Base,
    /// `[base + disp]`.
    BaseDisp,
    /// `[base + index]`.
    BaseIndex,
    /// `[base + index * scale]`.
    BaseIndexScale,
    /// `[base + index * scale + disp]`.
    BaseIndexScaleDisp,
}

impl AddrMode {
    /// Every mode, in order.
    pub const ALL: [Self; 5] = [
        Self::Base,
        Self::BaseDisp,
        Self::BaseIndex,
        Self::BaseIndexScale,
        Self::BaseIndexScaleDisp,
    ];

    /// Where this mode sits in a mode-indexed field.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// How many structural features this mode has, per section 40.9.
    ///
    /// One each for a displacement, an index, and a scale on the index. The base is not counted
    /// because every mode has one, and a feature every mode has cannot discriminate between them.
    ///
    /// This is the absolute count. Section 40.9's refinement, that a feature the target has no
    /// alternative to should not count against a mode, is applied by [`CostTable::addr_cost`],
    /// which is where the target is known.
    #[must_use]
    pub const fn complexity(self) -> u32 {
        match self {
            Self::Base => 0,
            Self::BaseDisp | Self::BaseIndex => 1,
            Self::BaseIndexScale => 2,
            Self::BaseIndexScaleDisp => 3,
        }
    }

    /// Whether this mode scales its index.
    #[must_use]
    pub const fn scales(self) -> bool {
        matches!(self, Self::BaseIndexScale | Self::BaseIndexScaleDisp)
    }
}

/// Which entries of a field are impossible, for the check that two tables agree about capability.
///
/// Section 40.13: "The two tables must differ only in numbers, never in capability, and that is
/// checkable." A capability is spelled [`Cycles::INFINITE`] in this design, so the check is that
/// the same entries are infinite in both tables, and that is what this reports. A field that
/// carries no capability, a count or a size, reports nothing rather than reporting false, so that
/// a count of 8 for speed and 4 for size is a difference in numbers and passes.
pub trait Capability {
    /// One entry per costed lane, saying whether that lane is impossible.
    fn impossible(&self) -> Vec<bool>;
}

impl Capability for Cycles {
    fn impossible(&self) -> Vec<bool> {
        vec![self.is_infinite()]
    }
}

impl<const N: usize> Capability for [Cycles; N] {
    fn impossible(&self) -> Vec<bool> {
        self.iter().map(|c| c.is_infinite()).collect()
    }
}

impl Capability for u32 {
    fn impossible(&self) -> Vec<bool> {
        Vec::new()
    }
}

impl Capability for Bytes {
    fn impossible(&self) -> Vec<bool> {
        Vec::new()
    }
}

/// Declares the cost table, its builder, and the two things that make it checkable.
///
/// A macro rather than a hand written struct because the completeness check needs the field list
/// and a hand written list is one somebody adds a field without updating. The field names exist
/// three times in the output and once in the source, which is the point.
macro_rules! cost_table {
    ($( $(#[$meta:meta])* $name:ident : $ty:ty ),+ $(,)?) => {
        /// What an operation costs on one target at one optimization goal.
        ///
        /// Built through [`Builder`] and no other way, per the module documentation. Every field
        /// is public to read and none of them can be written after the table exists, because a
        /// target's costs are data and a pass that adjusts them is a pass keeping a policy
        /// somewhere nobody can find it.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct CostTable {
            $( $(#[$meta])* pub $name: $ty, )+
        }

        impl CostTable {
            /// Every field name, in declaration order.
            pub const FIELDS: &'static [&'static str] = &[ $( stringify!($name) ),+ ];

            /// Which entries of which fields are impossible, per [`Capability`].
            #[must_use]
            pub fn capabilities(&self) -> Vec<(&'static str, Vec<bool>)> {
                vec![ $( (stringify!($name), Capability::impossible(&self.$name)) ),+ ]
            }
        }

        /// The one way to build a [`CostTable`].
        ///
        /// Every field starts unset, and [`Builder::build`] refuses to produce a table while any
        /// of them still is. That is section 40.13's completeness check, moved from a test into
        /// the constructor so that a target added next year cannot skip it.
        #[derive(Debug, Clone, Default)]
        pub struct Builder {
            $( $name: Option<$ty>, )+
        }

        impl Builder {
            /// A table with nothing set yet.
            #[must_use]
            pub fn new() -> Self {
                Self { $( $name: None, )+ }
            }

            $(
                $(#[$meta])*
                // One field is called `add`, so one setter is called `add`, and clippy reads that
                // as a `std::ops::Add` somebody spelled wrong. The setter is named after the field
                // and the field is named after the operation, which is the property worth keeping.
                #[allow(clippy::should_implement_trait)]
                #[must_use]
                pub fn $name(mut self, value: $ty) -> Self {
                    self.$name = Some(value);
                    self
                }
            )+

            /// The fields nobody has set, in declaration order.
            ///
            /// Public so that a test can name them, which turns "the table is incomplete" into
            /// "the table is missing `branch_cost`" without anybody reading a panic message.
            #[must_use]
            pub fn missing(&self) -> Vec<&'static str> {
                let mut missing = Vec::new();
                $( if self.$name.is_none() { missing.push(stringify!($name)); } )+
                missing
            }

            /// The finished table.
            ///
            /// # Panics
            ///
            /// If any field was left unset, naming them. A target's cost table is written once and
            /// is a compile time constant of the compiler in every sense that matters, so this
            /// fires during the tests of whoever added the target and never in front of a user.
            #[must_use]
            pub fn build(self) -> CostTable {
                let missing = self.missing();
                assert!(
                    missing.is_empty(),
                    "the cost table is missing {} of its {} fields: {}. \
                     A field left unset would be a zero, and a zero cost makes an operation free.",
                    missing.len(),
                    CostTable::FIELDS.len(),
                    missing.join(", "),
                );
                CostTable {
                    $( $name: self.$name.expect("checked just above"), )+
                }
            }
        }
    };
}

cost_table! {
    /// A register to register add, which is the operation [`Cycles::ONE`] is defined as.
    ///
    /// It is in the table anyway rather than assumed to be one, because a target where the unit
    /// operation is not an add should say so instead of having its whole table shifted.
    add: Cycles,

    /// An address computation that does not touch flags, x86-64's `lea`.
    ///
    /// Section 40.3 keeps this separate from `add` because whether it is cheaper is exactly the
    /// kind of microarchitectural fact that varies between cores of the same target.
    lea: Cycles,

    /// A shift by an amount known at compile time.
    shift_const: Cycles,

    /// A shift by an amount in a register, which on x86-64 is the expensive one because of the
    /// flags dependency and the fixed count register.
    shift_var: Cycles,

    /// A multiply, indexed by [`Width`].
    mult: [Cycles; 4],

    /// What each set bit in a constant multiplier adds, for deciding when to expand a multiply by
    /// a constant into shifts and adds.
    mult_bit: Cycles,

    /// A divide, indexed by [`Width`]. The most expensive integer operation on every target and
    /// the reason strength reduction of division is worth doing at all.
    divide: [Cycles; 4],

    /// A sign extension.
    movsx: Cycles,

    /// A zero extension, which on x86-64 is free for the 32 to 64 case and is not for the others,
    /// so this is the cost of the ones that are not free.
    movzx: Cycles,

    /// A register to register move, as the expression evaluator sees it.
    reg_move: Cycles,

    /// An integer load, indexed by [`Width`], as the register allocator sees it.
    move_int_load: [Cycles; 4],

    /// An integer store, indexed by [`Width`], as the register allocator sees it.
    move_int_store: [Cycles; 4],

    /// A move between two integer registers, as the register allocator sees it.
    ///
    /// Separate from `reg_move` on purpose, per section 40.3. The allocator asks what a move it is
    /// about to insert costs, and the evaluator asks what a move already in the program costs, and
    /// `gcc/config/i386/i386.h:114` says plainly that the two answers can differ.
    move_int_reg: Cycles,

    /// A floating point load, for the two widths that exist, single then double.
    move_fp_load: [Cycles; 2],

    /// A floating point store, single then double.
    move_fp_store: [Cycles; 2],

    /// A move between two floating point registers.
    move_fp_reg: Cycles,

    /// A move from a floating point register to an integer one, which goes through memory or a
    /// dedicated instruction and is never free.
    move_fp_to_int: Cycles,

    /// A move from an integer register to a floating point one.
    move_int_to_fp: Cycles,

    /// An address of each shape, indexed by [`AddrMode`], per section 40.9.
    ///
    /// A mode the target does not have is [`Cycles::INFINITE`], which is what the check that the
    /// speed and size tables agree about capability reads.
    addr: [Cycles; 5],

    /// What an unpredictable branch costs when optimizing for speed, per section 40.5.
    ///
    /// Only the unpredictable case is a target number. `BRANCH_COST` at
    /// `gcc/config/i386/i386.h:2023` makes a predictable branch free and a branch costed for size
    /// worth 2 on every target, and those two are in [`crate::heuristics`] rather than here
    /// because they are not facts about the machine.
    branch_cost: Cycles,

    /// What a mispredicted branch costs, per section 40.10.
    ///
    /// The number that decides whether a switch becomes a jump table, because an indirect branch
    /// with many targets has to be priced as a mispredict and not as a branch.
    mispredict_penalty: Cycles,

    /// How many scalar moves a block copy may expand to before it becomes a call, per section 40.7.
    ///
    /// GCC's `move_ratio`. A count of moves rather than of bytes, because how many moves a copy
    /// takes depends on the alignment the compiler can prove.
    move_ratio: u32,

    /// The same for a block fill. GCC's `clear_ratio`.
    clear_ratio: u32,

    /// The narrowest store worth using, per section 40.7's trimming rule.
    ///
    /// A partially dead store is trimmed only to a width at least this wide. Narrowing an 8-byte
    /// store to a 1-byte store because seven bytes are dead is legal and is usually a store
    /// forwarding stall, which is the thing this number stops.
    cheapest_store: Bytes,

    /// How many integer operations the machine issues in parallel, per section 40.8.
    ///
    /// The reassociation width. A chain of eight adds becomes a tree only on a machine that can
    /// execute the tree's independent operations at once, so this is a hardware fact rather than a
    /// tuning constant, and it defaults to 1 on a new target, meaning no reassociation.
    reassoc_int: u32,

    /// The same for floating point.
    ///
    /// Reassociating floating point needs `-ffast-math` whatever this says, because the
    /// transformation is not value preserving. This is only how wide the tree may be once that
    /// question has been answered somewhere else.
    reassoc_fp: u32,
}

impl CostTable {
    /// A fresh builder, which is the only way to a table.
    #[must_use]
    pub fn builder() -> Builder {
        Builder::new()
    }

    /// What a multiply of this width costs.
    #[must_use]
    pub fn mult_of(&self, width: Width) -> Cycles {
        self.mult[width.index()]
    }

    /// What a divide of this width costs.
    #[must_use]
    pub fn divide_of(&self, width: Width) -> Cycles {
        self.divide[width.index()]
    }

    /// What an integer load of this width costs the allocator.
    #[must_use]
    pub fn int_load(&self, width: Width) -> Cycles {
        self.move_int_load[width.index()]
    }

    /// What an integer store of this width costs the allocator.
    #[must_use]
    pub fn int_store(&self, width: Width) -> Cycles {
        self.move_int_store[width.index()]
    }

    /// Whether the target has this addressing mode at all.
    #[must_use]
    pub fn has_addr(&self, mode: AddrMode) -> bool {
        !self.addr[mode.index()].is_infinite()
    }

    /// What an address of this shape costs, with the complexity counted relative to the target.
    ///
    /// Section 40.9's refinement, and the comment it comes from at
    /// `gcc/tree-ssa-loop-ivopts.cc:4799`: "Don't increase the complexity of adding a scaled index
    /// if it's the only kind of index that the target allows". A feature the target offers no
    /// alternative to is not a complication, and counting it as one makes every address on that
    /// target look complicated, which is the same as the tiebreak not working.
    #[must_use]
    pub fn addr_cost(&self, mode: AddrMode) -> crate::Cost {
        let cycles = self.addr[mode.index()];
        if cycles.is_infinite() {
            return crate::Cost::INFINITE;
        }
        let mut complexity = mode.complexity();
        // The scale is free to write down if there is no unscaled index mode to write instead.
        if mode.scales() && !self.has_addr(AddrMode::BaseIndex) {
            complexity -= 1;
        }
        crate::Cost::new(cycles, complexity)
    }
}

#[cfg(test)]
mod tests {
    use super::{AddrMode, Builder, CostTable, Width};
    use crate::{Bytes, Cost, Cycles};

    /// A table with every field set to something, for tests about the mechanism rather than the
    /// numbers. The numbers a real target uses are tested in that target's own module.
    fn filled() -> Builder {
        let one = Cycles::ONE;
        CostTable::builder()
            .add(one)
            .lea(one)
            .shift_const(one)
            .shift_var(one)
            .mult([one; 4])
            .mult_bit(one)
            .divide([one; 4])
            .movsx(one)
            .movzx(one)
            .reg_move(one)
            .move_int_load([one; 4])
            .move_int_store([one; 4])
            .move_int_reg(one)
            .move_fp_load([one; 2])
            .move_fp_store([one; 2])
            .move_fp_reg(one)
            .move_fp_to_int(one)
            .move_int_to_fp(one)
            .addr([one; 5])
            .branch_cost(one)
            .mispredict_penalty(one)
            .move_ratio(8)
            .clear_ratio(8)
            .cheapest_store(Bytes(4))
            .reassoc_int(1)
            .reassoc_fp(1)
    }

    #[test]
    fn a_full_table_builds() {
        let table = filled().build();
        assert_eq!(table.add, Cycles::ONE);
        assert!(!CostTable::FIELDS.is_empty());
    }

    #[test]
    fn an_empty_builder_is_missing_every_field() {
        assert_eq!(Builder::new().missing(), CostTable::FIELDS);
    }

    #[test]
    #[should_panic(expected = "branch_cost")]
    fn a_table_missing_a_field_does_not_build_and_says_which() {
        // The failure section 40.13 is about. It has to be impossible to reach a table with a
        // field nobody set, because that field would read as zero and a zero cost is free.
        let mut incomplete = filled();
        incomplete.branch_cost = None;
        let _ = incomplete.build();
    }

    #[test]
    fn setting_a_field_twice_keeps_the_second() {
        let table = filled().add(Cycles::insns(7)).build();
        assert_eq!(table.add, Cycles::insns(7));
    }

    #[test]
    fn every_field_name_is_distinct() {
        // The macro would happily declare two fields with the same name and the compiler would
        // stop it, but the name list is what the completeness message prints, so it is worth
        // knowing that two entries in it never mean the same field.
        let mut names = CostTable::FIELDS.to_vec();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before);
    }

    #[test]
    fn widths_index_their_own_slots() {
        for (slot, width) in Width::ALL.iter().enumerate() {
            assert_eq!(width.index(), slot);
            assert_eq!(Width::from_bits(width.bits()), Some(*width));
        }
        assert_eq!(Width::from_bits(128), None);
        assert_eq!(Width::from_bits(1), None);
    }

    #[test]
    fn an_address_gets_one_point_of_complexity_per_feature() {
        assert_eq!(AddrMode::Base.complexity(), 0);
        assert_eq!(AddrMode::BaseDisp.complexity(), 1);
        assert_eq!(AddrMode::BaseIndex.complexity(), 1);
        assert_eq!(AddrMode::BaseIndexScale.complexity(), 2);
        assert_eq!(AddrMode::BaseIndexScaleDisp.complexity(), 3);
    }

    #[test]
    fn a_mode_the_target_lacks_is_impossible_rather_than_expensive() {
        let mut addrs = [Cycles::ONE; 5];
        addrs[AddrMode::BaseIndexScaleDisp.index()] = Cycles::INFINITE;
        let table = filled().addr(addrs).build();
        assert!(!table.has_addr(AddrMode::BaseIndexScaleDisp));
        assert_eq!(table.addr_cost(AddrMode::BaseIndexScaleDisp), Cost::INFINITE);
        assert!(table.has_addr(AddrMode::Base));
    }

    #[test]
    fn a_scale_the_target_has_no_alternative_to_does_not_count_as_a_complication() {
        // Section 40.9's refinement. On a machine whose only index mode is scaled, an address
        // with a scaled index is the plain one, and charging it for the scale would make every
        // address on that target tie at the same complexity.
        let mut addrs = [Cycles::ONE; 5];
        addrs[AddrMode::BaseIndex.index()] = Cycles::INFINITE;
        let scaled_only = filled().addr(addrs).build();
        assert_eq!(scaled_only.addr_cost(AddrMode::BaseIndexScale).complexity, 1);

        let both = filled().build();
        assert_eq!(both.addr_cost(AddrMode::BaseIndexScale).complexity, 2);
    }
}
