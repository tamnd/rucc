//! `__attribute__((sseregparm))`, gcc's word on 32-bit x86 that a function takes its `float` and
//! `double` arguments in SSE registers and returns one in `xmm0`, rather than on the stack and on
//! the x87 stack.
//!
//! gcc reads it off the function type, so it lands where a calling convention does, and it is
//! [`FunctionType::sse_regparm`] here. This compiler builds i386 code with no extensions, so there
//! is never an SSE register to put a float in, and gcc built that way refuses each definition of
//! such a function and each call to one in the words used here. Refusing is the point: dropping
//! the attribute in silence would pass a float on the stack to a callee gcc built to look in
//! `xmm0`.

use rucc_ast::AttrList;
use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::{FunctionType, TypeId, TypeKind, pointee};

use crate::check::Checker;
use crate::expr::{Conversion, ExprId, ExprKind};

impl Checker<'_> {
    /// The type with its function, or the function it points at, made one that takes its floats
    /// in SSE registers where the list says `sseregparm`, and the type as written where it does
    /// not or gcc drops it.
    ///
    /// Each one written is held to gcc's rules in turn: no arguments, which is an error otherwise,
    /// then a function type, then 32-bit x86. The last two are warnings that drop it, and on
    /// x86-64 Windows gcc drops it without a word, since the attribute names nothing there.
    ///
    /// `pointee` is a type a pointer's own list landed on, which is already the function and goes
    /// no further down.
    pub(in crate::check) fn sse_registers(
        &mut self,
        ty: TypeId,
        attrs: AttrList,
        pointee: bool,
    ) -> TypeId {
        let tuple = self.cx.target.tuple;
        let wide = match tuple.arch().as_str() {
            "x86_64" => true,
            "i686" => false,
            _ => return ty,
        };
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
        let mut asked = false;
        for &attr in &ast[attrs] {
            if self.gnu_name(&attr) != "sseregparm" {
                continue;
            }
            let count = ast[attr.args].len();
            if count > 0 {
                let what = "wrong number of arguments specified for 'sseregparm' attribute";
                let refused = Diagnostic::error(what, attr.span).with_code("E0841");
                self.report(refused.note(format!("expected 0, found {count}"), attr.span));
                continue;
            }
            if function.is_none() {
                self.not_a_function("sseregparm", attr.span);
                continue;
            }
            if wide {
                if tuple.os().as_str() != "windows" {
                    let what = "'sseregparm' attribute ignored";
                    let note = "the attribute names a calling convention of 32-bit x86, which \
                                x86-64 outside Windows does not have";
                    let dropped = Diagnostic::warning(what, attr.span).with_code("E0703");
                    self.report(dropped.note(note, attr.span));
                }
                continue;
            }
            asked = true;
        }
        let (true, Some((function, through))) = (asked, function) else { return ty };
        let current = self.types.signature(function);
        if current.sse_regparm {
            return ty;
        }
        let signature = FunctionType { sse_regparm: true, ..current.clone() };
        let sse = self.types.function(signature);
        if !through {
            return sse;
        }
        let quals = self.types.quals(canonical);
        let pointer = self.types.pointer(sse);
        self.types.qualified(pointer, quals)
    }

    /// Refuses the definition of a function of type `ty` that takes its floats in SSE registers,
    /// in the words gcc built without SSE uses, which name it as though it were being called.
    pub(in crate::check) fn sse_refused_definition(
        &mut self,
        ty: TypeId,
        name: Symbol,
        span: Span,
    ) {
        if self.takes_sse(ty) {
            let named = format!("'{}'", self.text(name));
            self.sse_refused(&named, span);
        }
    }

    /// Refuses a call through `callee` to a function that takes its floats in SSE registers, in
    /// gcc's words, which name the function where the call names one and its type where the call
    /// is through a pointer.
    pub(in crate::check) fn sse_refused_call(
        &mut self,
        callee: ExprId,
        signature: &FunctionType,
        span: Span,
    ) {
        if !signature.sse_regparm {
            return;
        }
        let named = match self.tast[callee].kind {
            ExprKind::Convert { kind: Conversion::FunctionDecay, operand } => {
                match self.tast[operand].kind {
                    ExprKind::Decl(decl) => self.tast[decl].name.map(|name| self.text(name)),
                    _ => None,
                }
                .map(|name| format!("'{name}'"))
            }
            _ => None,
        };
        let named = named.unwrap_or_else(|| {
            let pointee = pointee(&self.types, self.tast[callee].ty);
            format!("'{}'", self.spell(pointee.unwrap_or(self.tast[callee].ty)))
        });
        self.sse_refused(&named, span);
    }

    fn takes_sse(&self, ty: TypeId) -> bool {
        match self.types.kind(self.types.canonical(ty)) {
            TypeKind::Function(function) => self.types.signature(function).sse_regparm,
            _ => false,
        }
    }

    fn sse_refused(&mut self, named: &str, span: Span) {
        let what = format!("calling {named} with attribute sseregparm without SSE/SSE2 enabled");
        let note = "this compiler builds i386 code with no extensions to the instruction set, so \
                    there is no SSE register to pass a float in";
        self.report(Diagnostic::error(what, span).with_code("E0842").note(note, span));
    }
}
