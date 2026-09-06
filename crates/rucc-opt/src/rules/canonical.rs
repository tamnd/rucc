//! The tier three rewrite table.
//!
//! Everything below the module comment is generated from `rules/canonical.rules` by `rucc-rules`
//! when this crate is built, and none of it is in the repository. The rule file is the only place
//! the rules are written, which is what makes the table that is matched with and the table
//! `rucc-verify` proves things about the same table.
//!
//! To read the rules, read the rule file. This is a third table rather than more lines in either
//! of the first two because a tier is a separate file, and because this one is matched under a
//! plan of its own: a canonicalisation is only correct when the side it moves the constant to is
//! known not to hold a constant already, and the plan is where that is said.

include!(concat!(env!("OUT_DIR"), "/canonical.rs"));
