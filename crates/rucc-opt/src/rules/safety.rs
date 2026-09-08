//! The table of conditions under which a safety check does not have to happen.
//!
//! Everything below the module comment is generated from `rules/safety.rules` by `rucc-rules` when
//! this crate is built, and none of it is in the repository. The rule file is the only place the
//! rules are written, which is what makes the table that is matched with and the table
//! `rucc-verify` proves things about the same table.
//!
//! To read the rules, read the rule file. What is worth saying here is what makes this table
//! unlike the five above it. Those are rewrites and are matched against instructions. This one is
//! not: `crate::discharge` walks the dominator tree, works out that two checks are about one
//! address a constant distance apart, builds a term saying so, and asks this table whether that is
//! enough to drop the second check. The term it builds is not in the function and never was.
//!
//! That split is `spec/safe-memory/07-check-elimination.md` section 7.7, and the reason for it is
//! that the two halves fail differently. A wrong walk is a bug of the kind tests find, and section
//! 14.3's differential check accounting is what looks for it. A wrong removal condition is
//! arithmetic that is off at the ends of the type, produces the right answer on everything anybody
//! runs, and is a vulnerability in the one case nobody wrote down, so it is written where a solver
//! has to agree with it before the build finishes.

include!(concat!(env!("OUT_DIR"), "/safety.rs"));
