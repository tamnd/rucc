//! `__attribute__((target_clones(...)))`: one function built several times over, once for each
//! set of extensions the attribute names, with the processor picking one when the program starts.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4.
//!
//! gcc 16 on an x86-64 ELF target builds each version as a local function called the function's
//! name, a dot and the version's suffix, and makes the name itself an indirect function whose
//! resolver asks libgcc what the processor has. This module reads the attribute the way gcc reads
//! it and hands the lowering the versions in the order the resolver tries them, measured one case
//! at a time:
//!
//! * Every string is a comma separated list, so `"avx2,default"` is two versions, and a name
//!   written twice is built once.
//! * The names a version can be are the twenty extensions gcc's dispatcher can test for and the
//!   four psABI levels as `arch=x86-64`, `arch=x86-64-v2` and so on. Anything else is refused in
//!   gcc's words: an option the `target` attribute takes but the dispatcher does not, `no-avx2`
//!   or `tune=haswell`, is "not supported", and a name gcc has never heard of is "unknown".
//! * `arch=` naming a processor rather than a level is refused here, and not by gcc, which tests
//!   the processor's family and model. That is the one shape of the attribute not built yet.
//! * An attribute with one name only is dropped with gcc's warning, leaving the function built
//!   once, and otherwise `default` has to be one of the names and an empty name is refused.
//! * A `target` attribute beside it is dropped with gcc's warning, as is `always_inline`, and the
//!   attribute on anything but a function is dropped with the warning every other function-only
//!   attribute gets.
//! * Every target but x86-64 ELF is refused, Windows and macOS in gcc's words, since neither has
//!   indirect functions, and AArch64 and the 32-bit targets because their dispatch is not built.

use rucc_ast::{AttrArg, AttrList};
use rucc_diag::{Diagnostic, Span};
use rucc_target::Target;

use crate::check::Checker;
use crate::check::builtin::cpu::feature_word;
use crate::decl::DeclKind;
use crate::expr::ExprKind;
use crate::tast::Version;

/// The names a version can be, with gcc 16's priority for each and the number of the feature
/// libgcc sets when the processor has it.
///
/// The priority is gcc's `enum feature_priority`, and a version with a higher one is tried first.
/// The feature is gcc's `enum processor_features`, the number `__builtin_cpu_supports` reads.
const DISPATCHED: &[(&str, u8, u8)] = &[
    ("mmx", 1, 1),
    ("sse", 2, 3),
    ("sse2", 3, 4),
    ("arch=x86-64", 4, 95),
    ("sse3", 5, 5),
    ("ssse3", 6, 6),
    ("sse4a", 8, 11),
    ("sse4.1", 10, 7),
    ("sse4.2", 11, 8),
    ("popcnt", 13, 2),
    ("arch=x86-64-v2", 14, 96),
    ("aes", 15, 18),
    ("pclmul", 16, 19),
    ("avx", 17, 9),
    ("bmi", 19, 16),
    ("fma4", 21, 12),
    ("xop", 22, 13),
    ("fma", 24, 14),
    ("bmi2", 26, 17),
    ("avx2", 27, 10),
    ("arch=x86-64-v3", 29, 97),
    ("avx512f", 30, 15),
    ("arch=x86-64-v4", 32, 98),
    ("avx10.1", 34, 114),
];

impl Checker<'_> {
    /// The versions a `target_clones` attribute in the lists asks the function to be built in,
    /// tried first to last with `default` the last of them.
    ///
    /// Nothing when no list says it, when what it says was refused, which has been reported, and
    /// when it names one version only, which gcc builds as an ordinary function. The warnings
    /// for the attributes it overrides are given here, since only a function that is really
    /// cloned overrides them.
    pub(in crate::check) fn cloned(
        &mut self,
        lists: &[AttrList],
        kind: DeclKind,
    ) -> Option<Vec<Version>> {
        let ast = self.ast;
        let mut said = None;
        let mut names: Vec<(String, Span)> = Vec::new();
        let mut refused = false;
        for &list in lists {
            for &attr in &ast[list] {
                if attr.namespace.is_some_and(|ns| self.text(ns) != "gnu")
                    || self.gnu_name(&attr) != "target_clones"
                {
                    continue;
                }
                if kind != DeclKind::Function {
                    let what = "'target_clones' attribute ignored";
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                    continue;
                }
                // The last attribute stands, which is what gcc reads two of as.
                said = Some(attr.span);
                names.clear();
                refused = false;
                if ast[attr.args].is_empty() {
                    let what = "wrong number of arguments specified for 'target_clones' attribute";
                    let error = Diagnostic::error(what, attr.span).with_code("E0805");
                    self.report(error.note("expected 1 or more, found 0", attr.span));
                    refused = true;
                }
                for &arg in &ast[attr.args] {
                    let AttrArg::Expr(expr) = arg else { continue };
                    let checked = self.expr(expr);
                    let at = self.tast.expr_span(checked);
                    let ExprKind::Str(id) = self.tast[checked].kind else {
                        let what = "'target_clones' attribute argument not a string constant";
                        self.report(Diagnostic::error(what, at).with_code("E0805"));
                        refused = true;
                        continue;
                    };
                    let text: String = self.tast[id]
                        .elements
                        .iter()
                        .filter_map(|&unit| char::from_u32(unit))
                        .collect();
                    names.extend(text.split(',').map(|name| (name.to_owned(), at)));
                }
            }
        }
        let span = said?;
        if refused {
            return None;
        }
        // Counted before anything in the names is judged, so `"avx2"` alone is this and not a
        // missing `default`, which is the order gcc says them in.
        if names.len() == 1 {
            let what = "single 'target_clones' attribute is ignored";
            self.report(Diagnostic::warning(what, span).with_code("E0703"));
            return None;
        }
        let mut versions: Vec<(u8, Version)> = Vec::new();
        let mut default = false;
        for (name, at) in &names {
            if versions.iter().any(|(_, version)| version.suffix == suffix(name))
                || (default && name == "default")
            {
                continue;
            }
            if name.is_empty() {
                let what = "an empty string cannot be in 'target_clones' attribute";
                self.report(Diagnostic::error(what, *at).with_code("E0805"));
                refused = true;
                continue;
            }
            if name == "default" {
                default = true;
                continue;
            }
            if let Some(&(_, priority, feature)) = DISPATCHED.iter().find(|row| row.0 == name) {
                let mut target = Target::new();
                if let Err(why) = target.read(name) {
                    self.report(Diagnostic::error(why.to_string(), *at).with_code("E0805"));
                    refused = true;
                    continue;
                }
                let version = Version {
                    suffix: suffix(name),
                    isa: Some(target.over(self.cx.isa)),
                    test: Some(feature_word(feature)),
                };
                versions.push((priority, version));
                continue;
            }
            refused = true;
            if let Some(cpu) = name.strip_prefix("arch=") {
                let what = format!(
                    "'target_clones' version 'arch={cpu}' is not built yet: only the psABI \
                     levels 'arch=x86-64' to 'arch=x86-64-v4' are dispatched on"
                );
                self.report(Diagnostic::error(what, *at).with_code("E0806"));
            } else if Target::new().read(name).is_ok() {
                let what = format!(
                    "ISA '{name}' is not supported in 'target' attribute, use 'arch=' syntax"
                );
                self.report(Diagnostic::error(what, *at).with_code("E0805"));
            } else {
                let what = format!("attribute 'target_clone' argument '{name}' is unknown");
                self.report(Diagnostic::error(what, *at).with_code("E0805"));
            }
        }
        if refused {
            return None;
        }
        if !default {
            self.report(Diagnostic::error("'default' target was not set", span).with_code("E0805"));
            return None;
        }
        // `"default,default"`, which gcc builds as one function after the same warning.
        if versions.is_empty() {
            let what = "single 'target_clones' attribute is ignored";
            self.report(Diagnostic::warning(what, span).with_code("E0703"));
            return None;
        }
        if !self.dispatches(span) {
            return None;
        }
        for &list in lists {
            for &attr in &ast[list] {
                let name = self.gnu_name(&attr);
                if name == "target" || name == "always_inline" {
                    let what = format!(
                        "ignoring attribute '{name}' because it conflicts with attribute \
                         'target_clones'"
                    );
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                }
            }
        }
        // Stable, so two names gcc gives one priority keep the order they were written in.
        versions.sort_by_key(|row| std::cmp::Reverse(row.0));
        let mut versions: Vec<Version> = versions.into_iter().map(|(_, version)| version).collect();
        versions.push(Version { suffix: "default".to_owned(), isa: None, test: None });
        Some(versions)
    }

    /// Whether the target can pick a version at run time, which takes an indirect function and
    /// the dispatch this compiler has built, and so far is x86-64 ELF only. Says why not if not.
    fn dispatches(&mut self, span: Span) -> bool {
        let tuple = &self.cx.target.tuple;
        let (arch, os) = (tuple.arch().as_str(), tuple.os().as_str());
        if matches!(os, "windows" | "macos") {
            let what = "the call requires 'ifunc', which is not supported by this target";
            self.report(Diagnostic::error(what, span).with_code("E0807"));
            return false;
        }
        if arch != "x86_64" {
            let what = format!("'target_clones' is not built for {arch} yet, only for x86-64");
            self.report(Diagnostic::error(what, span).with_code("E0807"));
            return false;
        }
        true
    }
}

/// What goes after the dot in a version's name: the name with every `=`, `-` and `.` an
/// underscore, which is how gcc makes `arch=x86-64-v3` into `arch_x86_64_v3`.
fn suffix(name: &str) -> String {
    name.replace(['=', '-', '.'], "_")
}
