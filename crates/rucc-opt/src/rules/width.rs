//! The tier four rewrite table.
//!
//! Everything below the module comment is generated from `rules/width.rules` by `rucc-rules` when
//! this crate is built, and none of it is in the repository. The rule file is the only place the
//! rules are written, which is what makes the table that is matched with and the table
//! `rucc-verify` proves things about the same table.
//!
//! To read the rules, read the rule file. This is a fourth table rather than more lines in one of
//! the first three because a tier is a separate file, and because this one is the first whose
//! patterns are about two instructions at once: it is matched under a plan that expands an operand
//! into the instruction that computed it, which none of the tiers above it wants.

include!(concat!(env!("OUT_DIR"), "/width.rs"));
