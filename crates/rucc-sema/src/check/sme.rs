//! The keyword attributes of the AArch64 Scalable Matrix Extension: `__arm_streaming`,
//! `__arm_streaming_compatible`, `__arm_in`, `__arm_out`, `__arm_inout`, `__arm_preserves`,
//! `__arm_new` and `__arm_locally_streaming`.
//!
//! gcc 14 defines each of them as a macro for a scoped attribute, `__arm_streaming` as
//! `[[__extension__ arm::streaming]]` and so on, on every AArch64 target and with or without SME.
//! The first six are part of the function type, so they land where a calling convention does and
//! they are [`FunctionType::sme`] here. The last two are about one definition and are read there.
//!
//! This compiler builds AArch64 code without SME, as gcc does when it is not asked for `+sme`. gcc
//! built that way accepts the attributes in declarations and types, holds them against a type
//! without them, and refuses what it cannot build in the words used here: a definition that runs
//! in streaming mode or has SME state, and a call that needs streaming mode or the caller's ZA
//! array or ZT0. A `__arm_streaming_compatible` function is built as an ordinary one, which is
//! correct, because without SME the processor is never in streaming mode.

use rucc_ast::{AttrArg, AttrList, Attribute};
use rucc_diag::{Diagnostic, Span};
use rucc_lex::Encoding;
use rucc_types::{FunctionType, Sme, SmeMode, SmeShare, TypeId, TypeKind};

use crate::check::Checker;
use crate::expr::ExprKind;

/// The note on each refusal, which says why a correct program is refused.
const NO_SME: &str = "this compiler builds AArch64 code without the Scalable Matrix Extension, \
                      as gcc does without '+sme'";

impl Checker<'_> {
    /// The type with its function, or the function it points at, given the SME mode and the SME
    /// state the list asks for, and the type as written where it asks for none.
    ///
    /// Each attribute is held to gcc's rules: `streaming` and `streaming_compatible` take no
    /// arguments, the four state attributes take one or more strings that name `za` or `zt0`, a
    /// state is shared one way only, and the later of two modes is dropped with a warning. On
    /// anything but a function or a pointer to one the attribute is dropped with gcc's warning.
    ///
    /// `pointee` is a type a pointer's own list landed on, which is already the function and goes
    /// no further down.
    pub(in crate::check) fn sme_typed(
        &mut self,
        ty: TypeId,
        attrs: AttrList,
        pointee: bool,
    ) -> TypeId {
        if self.cx.target.tuple.arch().as_str() != "aarch64" {
            return ty;
        }
        let ast = self.ast;
        let canonical = self.types.canonical(ty);
        let function = match self.types.kind(canonical) {
            TypeKind::Function(function) => Some((function, false)),
            TypeKind::Pointer(to) if !pointee => match self.types.kind(self.types.canonical(to)) {
                TypeKind::Function(function) => Some((function, true)),
                _ => None,
            },
            _ => None,
        };
        let mut sme =
            function.map_or(Sme::NONE, |(function, _)| self.types.signature(function).sme);
        let before = sme;
        for &attr in &ast[attrs] {
            let Some(name) = self.arm_name(&attr) else { continue };
            let (mode, share) = match name {
                "streaming" => (Some(SmeMode::Streaming), None),
                "streaming_compatible" => (Some(SmeMode::Compatible), None),
                "in" => (None, Some(SmeShare::In)),
                "out" => (None, Some(SmeShare::Out)),
                "inout" => (None, Some(SmeShare::Inout)),
                "preserves" => (None, Some(SmeShare::Preserves)),
                _ => continue,
            };
            let count = ast[attr.args].len();
            if let Some(mode) = mode {
                if count > 0 {
                    let what = format!("'{name}' attribute does not take any arguments");
                    self.report(Diagnostic::error(what, attr.span).with_code("E0880"));
                    continue;
                }
                if function.is_none() {
                    self.not_a_function(name, attr.span);
                    continue;
                }
                match sme.mode {
                    SmeMode::Normal => sme.mode = mode,
                    held if held == mode => {}
                    held => {
                        let held = if held == SmeMode::Streaming {
                            "streaming"
                        } else {
                            "streaming_compatible"
                        };
                        let what = format!(
                            "ignoring attribute '{name}' because it conflicts with attribute \
                             '{held}'"
                        );
                        self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                    }
                }
                continue;
            }
            let Some(share) = share else { continue };
            if count == 0 {
                let what = format!("wrong number of arguments specified for '{name}' attribute");
                let refused = Diagnostic::error(what, attr.span).with_code("E0880");
                self.report(refused.note("expected at least 1, found 0", attr.span));
                continue;
            }
            let Some(states) = self.sme_states(&attr, name) else { continue };
            if function.is_none() {
                self.not_a_function(name, attr.span);
                continue;
            }
            for state in states {
                let held = match state.as_str() {
                    "za" => &mut sme.za,
                    "zt0" => &mut sme.zt0,
                    _ => {
                        let what = format!("unrecognized state string '{state}'");
                        self.report(Diagnostic::error(what, attr.span).with_code("E0880"));
                        continue;
                    }
                };
                match *held {
                    None => *held = Some(share),
                    Some(was) if was == share => {}
                    Some(_) => {
                        let what = format!("inconsistent attributes for state '{state}'");
                        self.report(Diagnostic::error(what, attr.span).with_code("E0880"));
                    }
                }
            }
        }
        let Some((function, through)) = function else { return ty };
        if sme == before {
            return ty;
        }
        let signature = FunctionType { sme, ..self.types.signature(function).clone() };
        let typed = self.types.function(signature);
        if !through {
            return typed;
        }
        let quals = self.types.quals(canonical);
        let pointer = self.types.pointer(typed);
        self.types.qualified(pointer, quals)
    }

    /// Refuses the definition of a function of type `ty` that runs in streaming mode or has SME
    /// state, in gcc's words, and says whether it did.
    ///
    /// `attrs` are the lists written on the definition, which is where `__arm_locally_streaming`
    /// and `__arm_new` are. gcc gives the streaming refusal alone when it applies, and otherwise a
    /// refusal for ZT0, which is SME2, before the one for SME state.
    pub(in crate::check) fn sme_refused_definition(
        &mut self,
        ty: TypeId,
        attrs: &[AttrList],
        span: Span,
    ) -> bool {
        if self.cx.target.tuple.arch().as_str() != "aarch64" {
            return false;
        }
        let mut sme = match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Function(function) => self.types.signature(function).sme,
            _ => return false,
        };
        let ast = self.ast;
        for &list in attrs {
            for &attr in &ast[list] {
                match self.arm_name(&attr) {
                    Some("locally_streaming") => sme.mode = SmeMode::Streaming,
                    Some("new") => {
                        for state in self.sme_states(&attr, "new").unwrap_or_default() {
                            match state.as_str() {
                                "za" => sme.za = Some(SmeShare::Inout),
                                "zt0" => sme.zt0 = Some(SmeShare::Inout),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if sme.mode == SmeMode::Streaming {
            self.sme_refused("streaming functions require the ISA extension 'sme'", span);
            return true;
        }
        if sme.zt0.is_some() {
            let what = "functions with 'zt0' state require the ISA extension 'sme2'";
            self.sme_refused(what, span);
        }
        if sme.shares() {
            self.sme_refused("functions with SME state require the ISA extension 'sme'", span);
        }
        sme.shares()
    }

    /// Refuses a call to a function that runs in streaming mode or shares SME state, in gcc's
    /// words.
    ///
    /// A caller that may share state is one whose own definition was refused, and gcc says
    /// nothing more about the calls in it, so the call is refused only from a caller with no SME
    /// state, which is every caller this compiler builds.
    pub(in crate::check) fn sme_refused_call(&mut self, signature: &FunctionType, span: Span) {
        if self.sme_quiet || self.defining.is_none() {
            return;
        }
        let sme = signature.sme;
        if sme.mode == SmeMode::Streaming {
            self.sme_refused("calling a streaming function requires the ISA extension 'sme'", span);
        }
        for (state, share) in [("za", sme.za), ("zt0", sme.zt0)] {
            match share {
                None => {}
                Some(SmeShare::Preserves) => self.sme_refused(
                    "call to a function that shares SME state from a function that has no SME \
                     state",
                    span,
                ),
                Some(_) => {
                    let what = format!(
                        "call to a function that shares '{state}' state from a function that has \
                         no '{state}' state"
                    );
                    self.sme_refused(&what, span);
                }
            }
        }
    }

    /// The name of an attribute written in the `arm` namespace, and [`None`] for any other.
    fn arm_name(&self, attr: &Attribute) -> Option<&'static str> {
        let namespace = attr.namespace?;
        if self.text(namespace) != "arm" {
            return None;
        }
        let name = match self.text(attr.name) {
            "streaming" => "streaming",
            "streaming_compatible" => "streaming_compatible",
            "locally_streaming" => "locally_streaming",
            "in" => "in",
            "out" => "out",
            "inout" => "inout",
            "preserves" => "preserves",
            "new" => "new",
            _ => return None,
        };
        Some(name)
    }

    /// The strings an SME state attribute names, or [`None`] after gcc's error when one of its
    /// arguments is not a string.
    fn sme_states(&mut self, attr: &Attribute, name: &str) -> Option<Vec<String>> {
        let args = self.ast[attr.args].to_vec();
        let mut states = Vec::with_capacity(args.len());
        for arg in args {
            let text = match arg {
                AttrArg::Expr(expr) => {
                    let checked = self.expr(expr);
                    match self.tast[checked].kind {
                        ExprKind::Str(id) if self.tast[id].encoding == Encoding::Plain => {
                            let elements = &self.tast[id].elements;
                            let text: String =
                                elements.iter().filter_map(|&unit| char::from_u32(unit)).collect();
                            Some(text.trim_end_matches('\0').to_owned())
                        }
                        _ => None,
                    }
                }
                AttrArg::Ident(_) => None,
            };
            let Some(text) = text else {
                let what = format!("the arguments to '{name}' must be constant strings");
                self.report(Diagnostic::error(what, attr.span).with_code("E0880"));
                return None;
            };
            states.push(text);
        }
        Some(states)
    }

    fn sme_refused(&mut self, what: &str, span: Span) {
        self.report(Diagnostic::error(what, span).with_code("E0881").note(NO_SME, span));
    }
}
