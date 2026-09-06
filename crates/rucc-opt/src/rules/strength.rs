//! The tier two rewrite table.
//!
//! Everything below the module comment is generated from `rules/strength.rules` by `rucc-rules`
//! when this crate is built, and none of it is in the repository. The rule file is the only place
//! the rules are written, which is what makes the table that is matched with and the table
//! `rucc-verify` proves things about the same table.
//!
//! To read the rules, read the rule file. This is a second table rather than more lines in the
//! first because a tier is a separate file, and because the two are tried in order: an identity
//! takes an operation away and a strength reduction swaps one for another, so a term both have
//! something to say about is better off losing the operation.

include!(concat!(env!("OUT_DIR"), "/strength.rs"));
