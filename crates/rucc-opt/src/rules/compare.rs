//! The tier five rewrite table.
//!
//! Everything below the module comment is generated from `rules/compare.rules` by `rucc-rules`
//! when this crate is built, and none of it is in the repository. The rule file is the only place
//! the rules are written, which is what makes the table that is matched with and the table
//! `rucc-verify` proves things about the same table.
//!
//! To read the rules, read the rule file. This is a fifth table rather than more lines in one of
//! the others because a tier is a separate file, and because this is the only one whose patterns
//! are comparisons: the predicate is not part of the opcode, so a rule here names one on the way
//! in and half of them name one on the way out, which nothing in the tiers above it does.

include!(concat!(env!("OUT_DIR"), "/compare.rs"));
