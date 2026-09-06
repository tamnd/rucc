//! The tier one rewrite table.
//!
//! Everything below the module comment is generated from `rules/simplify.rules` by `rucc-rules`
//! when this crate is built, and none of it is in the repository. The rule file is the only place
//! the rules are written, which is what makes the table that is matched with and the table
//! `rucc-verify` proves things about the same table.
//!
//! To read the rules, read the rule file. To read the automaton they compile into, build the
//! crate and read `simplify.rs` under the build directory, which is a file worth looking at once
//! for the shape of it and never again.

include!(concat!(env!("OUT_DIR"), "/simplify.rs"));
