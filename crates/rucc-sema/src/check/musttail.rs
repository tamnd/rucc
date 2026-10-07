//! `[[gnu::musttail]] return f(x);`, gcc 15's tail call that is not an optimization but a promise:
//! the call is made as a jump once this function's frame is given back, at every level of
//! optimization, and where it cannot be the compile stops and says why. An interpreter written
//! as one function per instruction, each ending in a call to the next, is what it is for. It runs
//! in the stack of one call however long the program, and without the jump it runs out of stack.
//!
//! Here is the front end's part, in gcc's words: the attribute goes on a `return` and nowhere
//! else, it takes no arguments, and what is returned has to be a call. The call is then kept in
//! [`crate::tast::Tast::is_must_tail`], and the walk to the IR marks it for the back end, which
//! makes the jump or says in gcc's words why it could not.
//!
//! The jump gives the frame back before the callee runs, so a pointer into it that the call is
//! handed points at nothing by then. gcc 15 warns about one passed as an argument, which is
//! `-Wmusttail-local-addr` and on by default, and so does this.
//!
//! A call that is not handed one may still reach the frame through an address the function gave
//! away earlier, and gcc 15 warns about that under `-Wmaybe-musttail-local-addr`, which `-Wextra`
//! turns on, once for each call the first warning said nothing about. It names one object: of
//! the variables whose address was taken and which are still in scope at the call, the one
//! declared last, and where there is none, the first parameter whose address was taken anywhere
//! in the function. That is gcc's reading without optimization, where any object whose address
//! was taken may be what a call reaches, and its words: `address of automatic variable 'a' can
//! escape to 'musttail' call`, or of `parameter 'x'`. An address is taken by `&` and by an array
//! decaying to a pointer, but not by an array being indexed, and a variable counts once that has
//! been written, which is where gcc's reading of what is live at the call comes from. So the
//! parameters are only named once the whole function has been read, and these warnings are
//! said then, in the order of the calls.
//!
//! gcc reads the frame once its optimizers have, and with them it names only the objects whose
//! address could have left the function, where this names any whose address was taken. A
//! compound literal, which gcc names as a local variable, is not named here. Nor is the second
//! warning given for a call the first one was given for under `-Wno-musttail-local-addr`, as gcc
//! gives it there.

use rucc_ast::{AttrList, Attribute, UnaryOp};
use rucc_diag::{Diagnostic, Span};
use rucc_types::is_pointer;

use crate::check::Checker;
use crate::check::stmt::Body;
use crate::decl::{DeclId, DeclKind, StorageDuration};
use crate::expr::{Conversion, ExprId, ExprKind};
use crate::scope::Binding;

/// A `musttail` call `-Wmusttail-local-addr` said nothing about, waiting for the end of its
/// function to be warned about under `-Wmaybe-musttail-local-addr` or not.
#[derive(Debug)]
pub(in crate::check) struct Tail {
    /// Where the call is.
    at: Span,
    /// The variable in scope at the call whose address was taken that gcc names, if any.
    local: Option<DeclId>,
}

impl Checker<'_> {
    /// Whether the attributes in front of a `return` ask for a tail call, with what gcc says about
    /// the ones that are wrong.
    ///
    /// gcc takes `musttail` out of the list and says nothing more of anything left in it than that
    /// the two were mixed. A list without it is one this compiler has nowhere to keep, as on any
    /// other statement.
    pub(in crate::check) fn musttail(&mut self, attrs: AttrList, span: Span) -> bool {
        let ast = self.ast;
        let mut asked = false;
        let mut others = false;
        for &attr in &ast[attrs] {
            if !self.is_musttail(&attr) {
                others = true;
                continue;
            }
            asked = true;
            if !ast[attr.args].is_empty() {
                let what = "'musttail' attribute does not take any arguments";
                self.report(Diagnostic::error(what, attr.span).with_code("E0847"));
            }
        }
        if asked && others {
            let what = "attribute 'musttail' mixed with other attributes on 'return' statement";
            self.report(Diagnostic::warning(what, span).with_code("E0703"));
        } else if others {
            self.report(
                Diagnostic::warning("attributes on this statement are ignored", span)
                    .with_code("E0411"),
            );
        }
        asked
    }

    /// Keeps the call a `musttail` return gives back, or says it is not one.
    ///
    /// gcc asks before the value is converted to the return type, so a call whose answer then has
    /// to change is taken here and refused by the back end, which says the value changed after
    /// the call.
    ///
    /// A function with variable arguments is refused here whether it reads them or not, since
    /// gcc refuses one by its type, before it looks at anything else about the call.
    pub(in crate::check) fn must_tail(&mut self, value: Option<ExprId>, span: Span) {
        let call = value.filter(|&value| matches!(self.tast[value].kind, ExprKind::Call { .. }));
        match call {
            Some(call) if self.in_variadic_function() => {
                let at = self.tast.expr_span(call);
                let what = "cannot tail-call: caller uses stdargs";
                self.report(Diagnostic::error(what, at).with_code("E0847"));
            }
            Some(call) => {
                self.tast.add_must_tail(call);
                self.musttail_local_addr(call);
            }
            None if value.is_some_and(|value| self.is_poisoned(value)) => {}
            None => {
                let what = "cannot tail-call: return value must be a call";
                self.report(Diagnostic::error(what, span).with_code("E0847"));
            }
        }
    }

    /// gcc's `-Wmusttail-local-addr`, once for each argument that is the address of something in
    /// this function's frame.
    ///
    /// gcc looks for it once the call is in its middle end, where an argument is a value or an
    /// address it can work out without running anything, so `&a`, `&s.m`, `&b[1]`, `b` decayed
    /// and any of them cast to another pointer are each one and `&b[i]` is not.
    fn musttail_local_addr(&mut self, call: ExprId) {
        let ExprKind::Call { args, .. } = self.tast[call].kind else { return };
        let span = self.tast.expr_span(call);
        let passed: Vec<String> =
            self.tast[args].iter().filter_map(|&arg| self.frame_address(arg)).collect();
        if passed.is_empty() {
            let local = self.escaped_local();
            if let Some(body) = &mut self.body {
                body.tails.push(Tail { at: span, local });
            }
        }
        for what in passed {
            let what = format!("address of {what} passed to 'musttail' call argument");
            self.report(Diagnostic::warning(what, span).with_code("E0850"));
        }
    }

    /// Of the variables whose address the function has taken so far and that are in scope here,
    /// the one gcc names, which is the one declared last.
    fn escaped_local(&self) -> Option<DeclId> {
        let body = self.body.as_ref()?;
        let decayed = body.decayed.iter().map(|&(_, decl)| decl);
        body.addressed
            .iter()
            .copied()
            .chain(decayed)
            .filter(|&decl| !self.is_parameter(decl))
            .filter(|&decl| {
                self.tast[decl].name.is_some_and(|name| {
                    self.scopes.lookup_where(name, |found| found == Binding::Decl(decl)).is_some()
                })
            })
            .max()
    }

    /// gcc's `-Wmaybe-musttail-local-addr`, for each `musttail` call of a function that has been
    /// read to its end.
    pub(in crate::check) fn maybe_musttail_local_addr(&mut self, body: &Body) {
        let params = &self.tast[body.params];
        let addressed = |decl: &DeclId| {
            body.addressed.contains(decl) || body.decayed.iter().any(|(_, held)| held == decl)
        };
        let param = params.iter().copied().find(addressed);
        let mut said = Vec::new();
        for tail in &body.tails {
            let what = match (tail.local, param) {
                (Some(local), _) => format!("automatic variable '{}'", self.decl_name(local)),
                (None, Some(param)) => format!("parameter '{}'", self.decl_name(param)),
                (None, None) => continue,
            };
            let what = format!("address of {what} can escape to 'musttail' call");
            said.push(Diagnostic::warning(what, tail.at).with_code("E0857"));
        }
        for diag in said {
            self.report(diag);
        }
    }

    /// The name an object of the frame was declared with.
    fn decl_name(&self, decl: DeclId) -> String {
        self.tast[decl].name.map_or_else(String::new, |name| self.text(name).to_owned())
    }

    /// Notes that the address of what an lvalue is part of was taken, where that is an object
    /// of this function's frame.
    pub(in crate::check) fn note_addressed(&mut self, lvalue: ExprId) {
        let Some(decl) = self.frame_root(lvalue) else { return };
        let Some(body) = &mut self.body else { return };
        if !body.addressed.contains(&decl) {
            body.addressed.push(decl);
        }
    }

    /// Notes a value that is an array of this function's frame decayed to a pointer, which takes
    /// its address unless it turns out to be the base of a subscript.
    pub(in crate::check) fn note_decayed(&mut self, value: ExprId) {
        let ExprKind::Convert { kind: Conversion::ArrayDecay, operand } = self.tast[value].kind
        else {
            return;
        };
        let Some(decl) = self.frame_root(operand) else { return };
        if let Some(body) = &mut self.body {
            body.decayed.push((value, decl));
        }
    }

    /// Takes back what [`Self::note_decayed`] noted of the base of a subscript.
    pub(in crate::check) fn note_subscripted(&mut self, base: ExprId) {
        if let Some(body) = &mut self.body {
            body.decayed.retain(|&(decay, _)| decay != base);
        }
    }

    /// The named object of this function's frame an lvalue is part of, through members and
    /// elements.
    fn frame_root(&self, lvalue: ExprId) -> Option<DeclId> {
        let mut at = lvalue;
        loop {
            match self.tast[at].kind {
                ExprKind::Member { base, .. } => at = base,
                ExprKind::Subscript { base, .. } => {
                    let ExprKind::Convert { kind: Conversion::ArrayDecay, operand } =
                        self.tast[base].kind
                    else {
                        return None;
                    };
                    at = operand;
                }
                ExprKind::Decl(decl) => {
                    let object = &self.tast[decl];
                    let automatic = object.kind == DeclKind::Object
                        && object.duration == StorageDuration::Automatic
                        && object.name.is_some();
                    return automatic.then_some(decl);
                }
                _ => return None,
            }
        }
    }

    /// What in this function's frame a pointer argument is the address of, in gcc's words.
    fn frame_address(&self, arg: ExprId) -> Option<String> {
        let mut at = arg;
        loop {
            let node = &self.tast[at];
            if !is_pointer(&self.types, node.ty) {
                return None;
            }
            match node.kind {
                ExprKind::Cast(operand)
                | ExprKind::Convert { kind: Conversion::Pointer, operand } => at = operand,
                ExprKind::LabelAddr(_) => return Some("label".to_owned()),
                ExprKind::Unary { op: UnaryOp::AddrOf, operand }
                | ExprKind::Convert { kind: Conversion::ArrayDecay, operand } => {
                    return self.frame_object(operand);
                }
                _ => return None,
            }
        }
    }

    /// The object in this function's frame an lvalue is part of, in gcc's words, where it is one
    /// at an offset known without running anything.
    fn frame_object(&self, lvalue: ExprId) -> Option<String> {
        let mut at = lvalue;
        loop {
            match self.tast[at].kind {
                ExprKind::Member { base, .. } => at = base,
                ExprKind::Subscript { base, index } => {
                    if !matches!(self.tast[index].kind, ExprKind::Const(_)) {
                        return None;
                    }
                    let ExprKind::Convert { kind: Conversion::ArrayDecay, operand } =
                        self.tast[base].kind
                    else {
                        return None;
                    };
                    at = operand;
                }
                ExprKind::Decl(decl) | ExprKind::CompoundLiteral(decl) => {
                    return self.automatic(decl);
                }
                _ => return None,
            }
        }
    }

    /// How gcc names an object of this function's: a parameter, a variable, or a compound
    /// literal, which has no name and is a local variable.
    fn automatic(&self, decl: DeclId) -> Option<String> {
        let object = &self.tast[decl];
        if object.kind != DeclKind::Object || object.duration != StorageDuration::Automatic {
            return None;
        }
        let name = object.name.map(|name| self.text(name));
        Some(match name {
            Some(name) if self.is_parameter(decl) => format!("parameter '{name}'"),
            Some(name) => format!("automatic variable '{name}'"),
            None => "local variable".to_owned(),
        })
    }

    /// gcc's warning for a `musttail` anywhere but in front of a `return`, which is that it is
    /// ignored.
    pub(in crate::check) fn musttail_misplaced(&mut self, lists: &[AttrList]) {
        let ast = self.ast;
        for &attrs in lists {
            for &attr in &ast[attrs] {
                if self.is_musttail(&attr) {
                    let what = "'musttail' attribute ignored";
                    self.report(Diagnostic::warning(what, attr.span).with_code("E0703"));
                }
            }
        }
    }

    /// Whether that is `musttail`, in any of gcc's spellings, `clang::musttail` among them.
    fn is_musttail(&self, attr: &Attribute) -> bool {
        if attr.namespace.is_some_and(|ns| self.text(ns) == "clang") {
            return self.gnu_name(&Attribute { namespace: None, ..*attr }) == "musttail";
        }
        self.gnu_name(attr) == "musttail"
    }
}
