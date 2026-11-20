//! The tier six rewrite table.
//!
//! Everything below the module comment is generated from `rules/select.rules` by `rucc-rules`
//! when this crate is built, and none of it is in the repository. The rule file is the only place
//! the rules are written, which is what makes the table that is matched with and the table
//! `rucc-verify` proves things about the same table.
//!
//! To read the rules, read the rule file. This is a sixth table rather than more lines in one of
//! the others because a tier is a separate file, and because it is the only one whose patterns are
//! selects: three operands, of which the first is one bit and the other two are the width of the
//! answer, and a replacement that writes instructions under the one it rewrites.

include!(concat!(env!("OUT_DIR"), "/select.rs"));
