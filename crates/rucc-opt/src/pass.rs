//! What a pass is, and the list of the ones this compiler has.

use rucc_ir::Func;

use crate::Fuel;

/// One transformation over one function.
///
/// A pass is a value rather than a function so that its name and its description travel with
/// it. The name is what `-fno-<name>`, `-fpass-fuel=<name>=<n>` and `-fdump-ir=after-<name>` all
/// spell, and there is one of it, which is why a pass cannot be added to a pipeline without
/// being reachable from the command line.
///
/// A pass sees one function at a time. Whole-module work is not this trait, and inlining will
/// need something else when it arrives.
pub trait Pass: Sync {
    /// What it is called, in lower case with hyphens between words.
    fn name(&self) -> &'static str;

    /// One line, for `--print-pipeline`.
    ///
    /// It says what the pass does to the code rather than how, because the reader of a pipeline
    /// listing is asking why their program came out the way it did.
    fn describe(&self) -> &'static str;

    /// Transforms the function, asking `fuel` before each transformation.
    ///
    /// Returns whether anything changed. A pass that says it changed nothing and did is a pass
    /// whose dumps lie, and one that says it changed something and did not costs a verifier run.
    fn run(&self, func: &mut Func, fuel: &mut Fuel) -> bool;
}

/// Every pass this compiler has, in no particular order.
///
/// The pipelines in [`crate::pipeline`] name passes out of this list, and `-f<name>` reaches any
/// of them whether or not the level asked for it. A pass that is written and not in here is a
/// pass nobody can turn on, so the list is the registry rather than a convenience.
pub static PASSES: &[&dyn Pass] = &[&crate::fold::Fold];

/// The pass with this name, if there is one.
#[must_use]
pub fn find(name: &str) -> Option<&'static dyn Pass> {
    PASSES.iter().copied().find(|pass| pass.name() == name)
}

#[cfg(test)]
mod tests {
    use super::PASSES;

    #[test]
    fn every_pass_has_a_name_a_flag_could_carry() {
        for pass in PASSES {
            let name = pass.name();
            assert!(!name.is_empty(), "a pass with no name cannot be turned off");
            assert!(
                name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'),
                "`{name}` is not spelled the way a -f flag is"
            );
            assert!(!pass.describe().is_empty(), "`{name}` says nothing about itself");
        }
    }

    #[test]
    fn no_two_passes_share_a_name() {
        for (index, pass) in PASSES.iter().enumerate() {
            for other in &PASSES[index + 1..] {
                assert_ne!(pass.name(), other.name(), "two passes answer to one name");
            }
        }
    }

    #[test]
    fn a_pass_is_found_by_its_name_and_nothing_else_is() {
        for pass in PASSES {
            assert_eq!(super::find(pass.name()).map(super::Pass::name), Some(pass.name()));
        }
        assert!(super::find("no-such-pass").is_none());
    }
}
