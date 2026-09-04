//! The verifier: whether a module is one the rest of the compiler may believe.
//!
//! Design: `spec/08-ir.md` section 8.7.
//!
//! This is not a debug aid. It runs after every pass in a debug build, after every pass in CI,
//! and on demand with `-fverify-ir`, and a pass that produces IR failing it is a build failure
//! rather than a warning. The reason is that a pass which breaks an invariant does not usually
//! produce wrong output there and then. It produces IR that a later pass reads under an
//! assumption that no longer holds, and the wrong instruction comes out somewhere else
//! entirely. Finding it here is the difference between a two-line diagnosis and a week.
//!
//! # What it checks
//!
//! Dominance, so that every use is reached by its definition. Block parameter arity and types
//! against every branch that arrives. Terminator placement. Operand and result types, by the
//! rules in section 8.2. That no instruction refers to a value whose definition has been taken
//! out of the function. That `alloca` is in the entry block unless its size is dynamic. That an
//! ordering is one the operation can be asked for. That the metadata is a tree. That every flag
//! is one the opcode reads. That every block is reachable. There is no separate rule that `asm
//! goto` ends its block, because inline assembly with labels is a terminator and terminator
//! placement is the rule that says so.
//!
//! # What it does not check
//!
//! Whether a name resolves. A module is one translation unit and a symbol it calls or takes the
//! address of is usually defined in another, so an unresolved name is the linker's question,
//! not this one. What is checked is the part that is here: where a direct call names a function
//! this module also holds, the signature at the call has to be the signature of that function.
//!
//! Side table indices are trusted. A `Sig` or the index of a `MemInfo` can only come from the
//! method that appended it, so a bad one is not a thing a pass can produce by accident. Values
//! and blocks are different, because a pass builds those by hand from indices it worked out
//! itself, so those are bounds-checked before anything else looks at them.
//!
//! # Errors, plural
//!
//! Every problem is reported, not just the first. The parser stops at the first thing that does
//! not add up because a malformed text has one author and one mistake. A module failing
//! verification was produced by a pass, and the shape of the whole failure is what says which
//! rewrite went wrong.

use std::fmt;

use rucc_base::Interner;
use rucc_target::{Slot, TargetInfo};

use crate::func::Func;
use crate::inst::{Abi, Block, CallInfo, Def, Inst, Param, Signature, VaInfo, Value};
use crate::module::{Alias, AliasKind, DataLayout, Datum, Global, Module, SymbolRef};
use crate::{Extra, MemOrder, Opcode, Type};

/// One thing wrong with a module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyError {
    /// What it is about, as `@sum block1 add`, or `@counter` for a global, or `!1` for a
    /// metadata node.
    pub at: String,
    /// What is wrong with it.
    pub message: String,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.at, self.message)
    }
}

impl std::error::Error for VerifyError {}

/// Checks a whole module.
///
/// # Errors
///
/// Gives back everything wrong with it, in the order the module is walked, which is the globals
/// then the aliases then the functions then the metadata.
pub fn verify(module: &Module, names: &Interner) -> Result<(), Vec<VerifyError>> {
    let mut verifier = Verifier::new(module, names);
    verifier.module();
    verifier.finish()
}

/// Checks one function, and nothing else in the module it is in.
///
/// This is what a pass calls after rewriting a function, since walking the whole module after
/// every function of it would be quadratic.
///
/// # Errors
///
/// Gives back everything wrong with that function.
pub fn verify_func<'a>(
    module: &'a Module,
    func: &'a Func,
    names: &'a Interner,
) -> Result<(), Vec<VerifyError>> {
    let mut verifier = Verifier::new(module, names);
    verifier.func(func);
    verifier.finish()
}

/// Checking one module.
struct Verifier<'a> {
    module: &'a Module,
    names: &'a Interner,
    errors: Vec<VerifyError>,
    /// Where the walk is, which is what an error is labelled with. Built into a string only
    /// when something is actually wrong, because the common case is that nothing is.
    func: Option<&'a Func>,
    block: Option<Block>,
    inst: Option<Inst>,
}

impl<'a> Verifier<'a> {
    fn new(module: &'a Module, names: &'a Interner) -> Self {
        Verifier { module, names, errors: Vec::new(), func: None, block: None, inst: None }
    }

    fn finish(self) -> Result<(), Vec<VerifyError>> {
        if self.errors.is_empty() { Ok(()) } else { Err(self.errors) }
    }

    // The module.

    fn module(&mut self) {
        let implied = DataLayout::for_target(&TargetInfo::new(self.module.triple));
        if self.module.datalayout != implied {
            self.at(
                format!("@{}", self.names.resolve(self.module.name)),
                format!(
                    "the datalayout is `{}` and {} implies `{implied}`",
                    self.module.datalayout, self.module.triple
                ),
            );
        }

        for id in self.module.globals() {
            self.global(&self.module[id]);
        }
        for id in self.module.aliases() {
            self.alias(&self.module[id]);
        }
        for id in self.module.funcs() {
            self.func(&self.module[id]);
        }
        self.metadata();
    }

    fn global(&mut self, global: &Global) {
        let at = format!("@{}", self.names.resolve(global.name));
        if !global.align.is_power_of_two() {
            self.at(
                at.clone(),
                format!("an alignment is a power of two and this is {}", global.align),
            );
        }
        // A global with no image is a declaration of something another module defines, and
        // there is nothing here to check about it. It may be constant: `extern const char
        // *const sys_errlist[];` is a declaration of an object that lives in the library's
        // read only data, and whether writing through a pointer to it is undefined is a fact
        // about the object rather than about which module holds the bytes.
        let Some(init) = global.init else { return };
        let mut size = 0;
        for &datum in &self.module[init] {
            size += datum.size(self.module);
            // An image is bytes and a scalar in one has to say how many it takes. `ptr` does
            // not: the width of an address is the target's and not the type's, which is what
            // makes a `ptr` here a scalar of no size that silently contributes nothing. An
            // address in an image is [`Datum::Addr`], and a number the program wrote as one is
            // the integer it is.
            if let Datum::Scalar { ty, .. } = datum {
                if ty.bits() == 0 {
                    self.at(
                        at.clone(),
                        format!("a scalar in an image has a width and {ty} has none"),
                    );
                }
            }
            if let Datum::Addr(reloc) = datum {
                let bytes = self.module[reloc].size;
                if !matches!(bytes, 1 | 2 | 4 | 8) {
                    self.at(
                        at.clone(),
                        format!(
                            "an address is written as 1, 2, 4 or 8 bytes and this one as {bytes}"
                        ),
                    );
                }
            }
        }
        if size != global.size {
            self.at(at, format!("the image is {size} bytes and the global is {}", global.size));
        }
    }

    fn alias(&mut self, alias: &Alias) {
        let at = format!("@{}", self.names.resolve(alias.name));
        if alias.name == alias.target {
            self.at(at, "an alias to itself");
            return;
        }
        // Only what this module holds is checked. A target defined in another object is what
        // an alias to a weak symbol looks like, and whether it resolves is the linker's answer.
        let Some(found) = self.module.lookup(alias.target) else { return };
        if alias.kind == AliasKind::IFunc && !matches!(found, SymbolRef::Func(_)) {
            self.at(at, "an ifunc resolves through a function and this target is not one");
        }
    }

    fn metadata(&mut self) {
        for node in self.module.metadata() {
            let Some(parent) = self.module[node].parent else { continue };
            if parent.raw() >= node.raw() {
                // A parent that comes later cannot be a tree, and a parent that is the node
                // itself is the cycle the walk up a TBAA tree would never leave.
                self.at(
                    format!("!{}", node.raw()),
                    format!(
                        "a metadata node's parent comes before it and this one is !{}",
                        parent.raw()
                    ),
                );
            }
        }
    }

    // The function.

    fn func(&mut self, func: &'a Func) {
        self.func = Some(func);
        self.block = None;
        self.inst = None;

        if let Some((one, other)) = func.attrs.conflict() {
            self.error(format!("`{one}` and `{other}` cannot both be true of a function"));
        }
        if func.is_declaration() {
            self.func = None;
            return;
        }
        // Everything after this indexes with values and blocks read out of the function, so
        // nothing does until they are all known to be in range.
        if !self.bounds(func) {
            self.func = None;
            return;
        }

        for signature in func.signatures() {
            self.signature(signature);
        }

        let entry = func.entry().expect("a function with blocks has a first one");
        let params: Vec<Type> = func[entry].params.iter().map(|&value| func[value].ty).collect();
        let want: Vec<Type> = func.signature().param_types().collect();
        if params != want {
            self.error(format!(
                "the entry block takes {} and the signature says {}",
                types(&params),
                types(&want)
            ));
        }

        let doms = Doms::new(func);
        let layout = Layout::new(func);
        for block in func.blocks() {
            self.block = Some(block);
            self.block(func, block, &doms, &layout);
        }
        self.block = None;
        self.inst = None;
        self.func = None;
    }

    /// What the ABI asks of a signature's parameters agrees with what they are.
    ///
    /// None of this is about the target. Whether a `struct` of twenty four bytes travels as its
    /// own bytes or as the address of a copy is the classification's answer and this has no
    /// opinion on it, but a `byval` on something that is not a pointer describes no call on any
    /// target, and neither does a second `sret`.
    fn signature(&mut self, signature: &Signature) {
        for (index, param) in signature.params.iter().enumerate() {
            let at = format!("parameter {}", index + 1);
            self.abi(&at, param);
            match param.abi {
                Abi::Sret { .. } if index > 0 => {
                    // It is the address the return value goes to, so it arrives before anything
                    // the function was called with. A later one is a different calling
                    // convention wearing the same word.
                    self.error("sret is the first parameter and this one is not");
                }
                Abi::Sret { .. } if !signature.returns.is_empty() => {
                    self.error("a signature returning through sret returns nothing else");
                }
                _ => {}
            }
        }
        for (index, param) in signature.returns.iter().enumerate() {
            let at = format!("result {}", index + 1);
            self.abi(&at, param);
            if param.abi.indirect() {
                // A return value too large for the registers comes back through an `sret`
                // parameter, which is a parameter and is checked as one.
                self.error(format!("{at} travels indirectly and a result cannot"));
            }
        }
    }

    /// What a call says about the arguments its signature does not name.
    ///
    /// The list is empty when they all travel as the values in hand, so what is checked is the
    /// other case: there is one entry for each of them, the call is variadic because nothing
    /// else has such an argument, and each entry describes something an argument can do. An
    /// `sret` cannot be one of them, since the space for a return value arrives first and the
    /// first argument is one the signature names.
    fn varargs(
        &mut self,
        func: &'a Func,
        info: &CallInfo,
        passed: usize,
        arg: impl Fn(usize) -> Type,
    ) {
        let varargs = &func[info.varargs];
        if varargs.is_empty() {
            return;
        }
        let named = func[info.signature].params.len();
        if !func[info.signature].variadic {
            self.error("this call is not variadic and says how a variadic argument travels");
            return;
        }
        if varargs.len() != passed.saturating_sub(named) {
            self.error(format!(
                "the call passes {} arguments the signature does not name and says how {} travel",
                passed.saturating_sub(named),
                varargs.len()
            ));
            return;
        }
        for (index, &abi) in varargs.iter().enumerate() {
            let at = format!("argument {}", named + index + 1);
            if matches!(abi, Abi::Sret { .. }) {
                self.error(format!("{at} is an sret and only a parameter can be one"));
                continue;
            }
            self.abi(&at, &Param { ty: arg(index + named), abi });
        }
    }

    /// One parameter's attribute against its type.
    fn abi(&mut self, at: &str, param: &Param) {
        match param.abi {
            Abi::Plain => {}
            Abi::Sext | Abi::Zext => {
                if !param.ty.is_int() || param.ty.is_vector() {
                    self.error(format!("{at} is extended and {} is not an integer", param.ty));
                }
            }
            Abi::ByVal { size, align } | Abi::Sret { size, align } => {
                if !param.ty.is_ptr() {
                    self.error(format!(
                        "{at} travels indirectly and {} is not a pointer",
                        param.ty
                    ));
                }
                if !align.is_power_of_two() {
                    self.error(format!("an alignment is a power of two and this is {align}"));
                }
                if size == 0 {
                    self.error(format!("{at} travels indirectly and has no size"));
                }
            }
        }
    }

    /// Every value and every block an instruction names is one the function has.
    ///
    /// This is separate and comes first because everything else reads a `ValueData` or a
    /// `BlockData` out of a table, and an index past the end of one is a panic rather than a
    /// diagnosis.
    fn bounds(&mut self, func: &'a Func) -> bool {
        let counts = func.counts();
        let before = self.errors.len();
        for block in func.blocks() {
            self.block = Some(block);
            for &value in &func[block].params {
                if value.index() >= counts.values {
                    self.error(format!(
                        "parameter %{} is not a value of this function",
                        value.raw()
                    ));
                }
            }
            for inst in func.insts(block) {
                self.inst = Some(inst);
                for &value in &func[func[inst].args] {
                    if value.index() >= counts.values {
                        self.error(format!("%{} is not a value of this function", value.raw()));
                    }
                }
                for value in func[inst].results() {
                    if value.index() >= counts.values {
                        self.error(format!("%{} is not a value of this function", value.raw()));
                    }
                }
                for call in func.successors(inst) {
                    if call.block.index() >= counts.blocks {
                        self.error(format!(
                            "block{} is not a block of this function",
                            call.block.raw()
                        ));
                    }
                    for &value in &func[call.args] {
                        if value.index() >= counts.values {
                            self.error(format!("%{} is not a value of this function", value.raw()));
                        }
                    }
                }
            }
            self.inst = None;
        }
        self.block = None;
        self.errors.len() == before
    }

    fn block(&mut self, func: &'a Func, block: Block, doms: &Doms, layout: &Layout) {
        if !doms.reaches(block) {
            self.error("this block is not reachable and has not been deleted");
        }
        if block == func.entry().expect("checked in func") {
            for other in func.blocks() {
                let last = func[other].last;
                if last.is_some_and(|inst| func.successors(inst).any(|call| call.block == block)) {
                    self.error("the entry block is branched to, and it takes the arguments");
                }
            }
        }

        let mut seen_terminator = false;
        for inst in func.insts(block) {
            self.inst = Some(inst);
            if seen_terminator {
                self.error("this comes after the block's terminator");
            }
            seen_terminator |= func.is_terminator(inst);
            self.inst(func, inst, doms, layout);
        }
        self.inst = None;
        if !seen_terminator {
            self.error("this block does not end in a terminator");
        }
    }

    fn inst(&mut self, func: &'a Func, inst: Inst, doms: &Doms, layout: &Layout) {
        let data = &func[inst];
        let opcode = data.opcode;

        let stray = data.flags.without(crate::Flags::legal_on(opcode));
        if !stray.is_empty() {
            let names: Vec<&str> = stray.iter().map(|(_, name)| name).collect();
            self.error(format!("{} does not read `{}`", opcode.name(), names.join("`, `")));
        }
        if data.extra.kind() != opcode.extra_kind() {
            // An instruction carrying some other opcode's payload prints as text the parser
            // cannot read, so this is caught here rather than found as a round trip failure.
            self.error(format!(
                "{} carries {} and this one carries {}",
                opcode.name(),
                opcode.extra_kind().name(),
                data.extra.kind().name()
            ));
        }
        if let Some(want) = opcode.results() {
            if want != data.results {
                self.error(format!(
                    "{} produces {want} values and this one produces {}",
                    opcode.name(),
                    data.results
                ));
            }
        }

        self.uses(func, inst, doms, layout);
        self.branches(func, inst);
        self.memory(func, inst);
        self.shape(func, inst);
    }

    /// Every use is reached by its definition.
    fn uses(&mut self, func: &'a Func, inst: Inst, doms: &Doms, layout: &Layout) {
        let block = layout.block_of(inst).expect("walking the blocks");
        let check = |verifier: &mut Self, value: Value| match func[value].def {
            Def::Param { block: def, .. } => {
                if !doms.dominates(def, block) {
                    verifier.error(format!(
                        "%{} arrives at block{} and does not reach here",
                        value.raw(),
                        def.raw()
                    ));
                }
            }
            Def::Result { inst: def, .. } => {
                let Some(def_block) = layout.block_of(def) else {
                    verifier.error(format!(
                        "%{} is produced by an instruction that is not in the function",
                        value.raw()
                    ));
                    return;
                };
                let reaches = if def_block == block {
                    layout.position(def) < layout.position(inst)
                } else {
                    doms.dominates(def_block, block)
                };
                if !reaches {
                    verifier.error(format!(
                        "%{} is produced in block{} and does not reach here",
                        value.raw(),
                        def_block.raw()
                    ));
                }
            }
        };
        for &value in &func[func[inst].args] {
            check(self, value);
        }
        for call in func.successors(inst) {
            for &value in &func[call.args] {
                check(self, value);
            }
        }
    }

    /// Every branch passes what the block it goes to takes.
    fn branches(&mut self, func: &'a Func, inst: Inst) {
        if func[inst].opcode == Opcode::BlockAddr {
            // The one instruction that names a block without arriving at it, so the block's
            // parameters are nothing to do with it. That it passes no arguments is checked
            // with the rest of its shape.
            return;
        }
        for call in func.successors(inst) {
            let params = &func[call.block].params;
            let args = &func[call.args];
            if params.len() != args.len() {
                self.error(format!(
                    "block{} takes {} arguments and this branch passes {}",
                    call.block.raw(),
                    params.len(),
                    args.len()
                ));
                continue;
            }
            for (index, (&param, &arg)) in params.iter().zip(args).enumerate() {
                let (want, got) = (func[param].ty, func[arg].ty);
                if want != got {
                    self.error(format!(
                        "argument {} to block{} is {want} and this one is {got}",
                        index + 1,
                        call.block.raw()
                    ));
                }
            }
        }
        if let Extra::Switch(info) = func[inst].extra {
            let switch = &func[info];
            let targets = &func[switch.targets];
            let cases = &func[switch.cases];
            if targets.len() != cases.len() + 1 {
                self.error(format!(
                    "a switch has one target per case and a default, and this one has {} targets for {} cases",
                    targets.len(),
                    cases.len()
                ));
            }
            for (index, case) in cases.iter().enumerate() {
                if cases[..index].contains(case) {
                    self.error("two cases of this switch have the same value");
                }
            }
        }
    }

    /// What an access says about itself.
    fn memory(&mut self, func: &'a Func, inst: Inst) {
        let opcode = func[inst].opcode;
        let info = match func[inst].extra {
            Extra::Mem(at) => func[at],
            Extra::Rmw(_, at) => func[at],
            Extra::VaObject(at) => {
                let object = func[at];
                self.slots(func, object);
                func[object.mem]
            }
            Extra::Order(order) => {
                if !order.is_valid_for_rmw() {
                    self.error("a fence is not a fence unless it orders something");
                }
                return;
            }
            _ => return,
        };
        if !info.align.is_power_of_two() {
            self.error(format!("an alignment is a power of two and this is {}", info.align));
        }
        if let Some(tbaa) = info.tbaa {
            if tbaa.index() >= self.module.counts().metadata {
                self.error(format!("!{} is not a metadata node of this module", tbaa.raw()));
            }
        }
        let ok = match opcode {
            Opcode::AtomicLoad => info.order.is_valid_for_load(),
            Opcode::AtomicStore => info.order.is_valid_for_store(),
            Opcode::AtomicRmw | Opcode::Cmpxchg => info.order.is_valid_for_rmw(),
            // Everything else is the non-atomic form, and the atomic form is a different
            // opcode, so an ordering here is one somebody meant to put on that one.
            _ => info.order == MemOrder::NotAtomic,
        };
        if !ok {
            self.error(format!("{} cannot be asked for {}", opcode.name(), info.order));
        }
        if matches!(opcode, Opcode::Memcpy | Opcode::Memmove | Opcode::Memset) && info.size == 0 {
            self.error(format!("{} moves no bytes", opcode.name()));
        }
    }

    /// Where an object read off a variable argument list travelled.
    ///
    /// Each slot holds bytes of the object, so a slot that reaches past the end of it is a
    /// classification that does not belong to this object and the backend would read memory the
    /// object never had.
    fn slots(&mut self, func: &'a Func, object: VaInfo) {
        let size = func[object.mem].size;
        for &slot in &func[object.slots] {
            let width = match slot {
                Slot::Integer { size, .. } => u64::from(size),
                Slot::Float { format, .. } => u64::from(format.width()).div_ceil(8),
            };
            if slot.offset() + width > size {
                self.error(format!(
                    "a slot holds bytes {} to {} of an object of {size} bytes",
                    slot.offset(),
                    slot.offset() + width
                ));
            }
        }
    }

    /// The operand and result types, by the rules of section 8.2.
    ///
    /// The shape of each group is written out rather than derived from a table, because the
    /// groups do not have the same shape as each other and a table general enough to hold all
    /// of them would be harder to read than this.
    #[expect(clippy::too_many_lines, reason = "one arm per group of opcodes, and they differ")]
    fn shape(&mut self, func: &'a Func, inst: Inst) {
        let data = &func[inst];
        let opcode = data.opcode;
        let args = &func[data.args];
        let arity = args.len();
        let arg = |n: usize| func[args[n]].ty;
        let results = usize::from(data.results);
        let res = |n: usize| func[data.results().nth(n).expect("within the count")].ty;

        match opcode {
            // Constants take nothing and say their own type.
            Opcode::IConst => {
                if self.takes(opcode, arity, 0) && results == 1 && !res(0).lane().is_int() {
                    self.error(format!(
                        "iconst produces an integer and this one produces {}",
                        res(0)
                    ));
                }
            }
            Opcode::FConst => {
                if self.takes(opcode, arity, 0) && results == 1 && !res(0).lane().is_float() {
                    self.error(format!(
                        "fconst produces a floating point value and this one produces {}",
                        res(0)
                    ));
                }
            }
            Opcode::Splat => {
                if self.takes(opcode, arity, 0) && results == 1 && !res(0).is_vector() {
                    self.error(format!("splat produces a vector and this one produces {}", res(0)));
                }
            }
            Opcode::GlobalAddr
            | Opcode::StackSave
            | Opcode::FrameAddress
            | Opcode::ReturnAddress => {
                if results == 1 && !res(0).is_ptr() {
                    self.error(format!(
                        "{} produces a pointer and this one produces {}",
                        opcode.name(),
                        res(0)
                    ));
                }
            }

            // Integer arithmetic: two of one type, and that type back.
            Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::SDiv
            | Opcode::UDiv
            | Opcode::SRem
            | Opcode::URem
            | Opcode::And
            | Opcode::Or
            | Opcode::Xor
            | Opcode::Shl
            | Opcode::LShr
            | Opcode::AShr => {
                if self.takes(opcode, arity, 2) {
                    self.integer(opcode, arg(0), 0);
                    self.agree(opcode, arg(0), arg(1));
                    if results == 1 {
                        self.produces(opcode, res(0), arg(0));
                    }
                }
            }

            // Floating point arithmetic, the same shape with one more operand for `fma`.
            Opcode::FAdd | Opcode::FSub | Opcode::FMul | Opcode::FDiv | Opcode::FRem => {
                if self.takes(opcode, arity, 2) {
                    self.floating(opcode, arg(0), 0);
                    self.agree(opcode, arg(0), arg(1));
                    if results == 1 {
                        self.produces(opcode, res(0), arg(0));
                    }
                }
            }
            Opcode::FNeg => {
                if self.takes(opcode, arity, 1) {
                    self.floating(opcode, arg(0), 0);
                    if results == 1 {
                        self.produces(opcode, res(0), arg(0));
                    }
                }
            }
            Opcode::Fma => {
                if self.takes(opcode, arity, 3) {
                    self.floating(opcode, arg(0), 0);
                    self.agree(opcode, arg(0), arg(1));
                    self.agree(opcode, arg(0), arg(2));
                    if results == 1 {
                        self.produces(opcode, res(0), arg(0));
                    }
                }
            }

            // A comparison answers one bit per lane, whatever it compared.
            Opcode::ICmp | Opcode::FCmp => {
                if self.takes(opcode, arity, 2) {
                    if opcode == Opcode::FCmp {
                        self.floating(opcode, arg(0), 0);
                    } else if !arg(0).lane().is_int() && !arg(0).is_ptr() {
                        self.error(format!(
                            "operand 1 of icmp is an integer or a pointer and this one is {}",
                            arg(0)
                        ));
                    }
                    self.agree(opcode, arg(0), arg(1));
                    if results == 1 {
                        self.produces(opcode, res(0), arg(0).with_lane(Type::I1));
                    }
                }
            }

            // Conversions, each of which says which way it goes and by how much.
            Opcode::Trunc | Opcode::SExt | Opcode::ZExt => {
                if self.takes(opcode, arity, 1) && results == 1 {
                    self.integer(opcode, arg(0), 0);
                    self.lanes(opcode, res(0), arg(0));
                    self.widens(opcode, res(0), arg(0), opcode != Opcode::Trunc);
                }
            }
            Opcode::FPTrunc | Opcode::FPExt => {
                if self.takes(opcode, arity, 1) && results == 1 {
                    self.floating(opcode, arg(0), 0);
                    self.lanes(opcode, res(0), arg(0));
                    self.widens(opcode, res(0), arg(0), opcode == Opcode::FPExt);
                }
            }
            Opcode::FPToSI | Opcode::FPToUI => {
                if self.takes(opcode, arity, 1) && results == 1 {
                    self.floating(opcode, arg(0), 0);
                    self.lanes(opcode, res(0), arg(0));
                    if !res(0).lane().is_int() {
                        self.error(format!(
                            "{} produces an integer and this one produces {}",
                            opcode.name(),
                            res(0)
                        ));
                    }
                }
            }
            Opcode::SIToFP | Opcode::UIToFP => {
                if self.takes(opcode, arity, 1) && results == 1 {
                    self.integer(opcode, arg(0), 0);
                    self.lanes(opcode, res(0), arg(0));
                    if !res(0).lane().is_float() {
                        self.error(format!(
                            "{} produces a floating point value and this one produces {}",
                            opcode.name(),
                            res(0)
                        ));
                    }
                }
            }
            Opcode::PtrToInt => {
                if self.takes(opcode, arity, 1) && results == 1 {
                    self.pointer(opcode, arg(0), 0);
                    self.integer(opcode, res(0), 0);
                }
            }
            Opcode::IntToPtr => {
                if self.takes(opcode, arity, 1) && results == 1 {
                    self.integer(opcode, arg(0), 0);
                    if !res(0).is_ptr() {
                        self.error(format!(
                            "inttoptr produces a pointer and this one produces {}",
                            res(0)
                        ));
                    }
                }
            }
            Opcode::Bitcast => {
                if self.takes(opcode, arity, 1) && results == 1 {
                    let (from, to) = (arg(0), res(0));
                    if from.is_ptr() != to.is_ptr() {
                        // Between an address and a number there is a conversion of its own,
                        // and using this one would hide it from anything looking for one.
                        self.error(
                            "a bitcast between a pointer and a number is ptrtoint or inttoptr",
                        );
                    } else if width(from) != width(to) {
                        self.error(format!("a bitcast keeps the width and {from} and {to} differ"));
                    }
                }
            }

            // Memory.
            Opcode::Alloca => {
                if arity > 1 {
                    self.takes(opcode, arity, 1);
                } else if arity == 1 {
                    self.integer(opcode, arg(0), 0);
                } else if func.block_of(inst) != func.entry() {
                    // One of a fixed size in a loop is a stack that grows every time round,
                    // which is what the dynamic form asks for explicitly.
                    self.error("an alloca of a fixed size belongs in the entry block");
                }
                if results == 1 && !res(0).is_ptr() {
                    self.error(format!(
                        "alloca produces a pointer and this one produces {}",
                        res(0)
                    ));
                }
            }
            Opcode::Load | Opcode::AtomicLoad => {
                if self.takes(opcode, arity, 1) {
                    self.pointer(opcode, arg(0), 0);
                }
                if results == 1 && res(0).is_void() {
                    self.error(format!("{} reads a value and void is not one", opcode.name()));
                }
            }
            Opcode::Store | Opcode::AtomicStore => {
                if self.takes(opcode, arity, 2) {
                    if arg(0).is_void() {
                        self.error(format!("{} writes a value and void is not one", opcode.name()));
                    }
                    self.pointer(opcode, arg(1), 1);
                }
            }
            Opcode::PtrAdd => {
                if self.takes(opcode, arity, 2) {
                    self.pointer(opcode, arg(0), 0);
                    self.integer(opcode, arg(1), 1);
                    if results == 1 && !res(0).is_ptr() {
                        self.error(format!(
                            "ptr_add produces a pointer and this one produces {}",
                            res(0)
                        ));
                    }
                }
            }
            Opcode::Memcpy | Opcode::Memmove => {
                if self.takes(opcode, arity, 2) {
                    self.pointer(opcode, arg(0), 0);
                    self.pointer(opcode, arg(1), 1);
                }
            }
            Opcode::Memset => {
                if self.takes(opcode, arity, 2) {
                    self.pointer(opcode, arg(0), 0);
                    self.integer(opcode, arg(1), 1);
                }
            }
            Opcode::AtomicRmw => {
                if self.takes(opcode, arity, 2) {
                    self.pointer(opcode, arg(0), 0);
                    self.integer(opcode, arg(1), 1);
                    if results == 1 {
                        self.produces(opcode, res(0), arg(1));
                    }
                }
            }
            Opcode::Cmpxchg => {
                if self.takes(opcode, arity, 3) {
                    self.pointer(opcode, arg(0), 0);
                    self.agree(opcode, arg(1), arg(2));
                    if results == 2 {
                        self.produces(opcode, res(0), arg(1));
                        self.produces(opcode, res(1), arg(1).with_lane(Type::I1));
                    }
                }
            }
            Opcode::Fence | Opcode::Unreachable | Opcode::UnreachableHint => {
                self.takes(opcode, arity, 0);
            }
            Opcode::Prefetch | Opcode::StackRestore | Opcode::VaStart | Opcode::VaEnd => {
                if self.takes(opcode, arity, 1) {
                    self.pointer(opcode, arg(0), 0);
                }
            }
            Opcode::VaCopy => {
                if self.takes(opcode, arity, 2) {
                    self.pointer(opcode, arg(0), 0);
                    self.pointer(opcode, arg(1), 1);
                }
            }
            Opcode::VaArg => {
                if self.takes(opcode, arity, 1) {
                    self.pointer(opcode, arg(0), 0);
                }
                if results == 1 && res(0).is_void() {
                    self.error("va_arg reads a value and void is not one");
                }
            }
            Opcode::VaObject => {
                if self.takes(opcode, arity, 1) {
                    self.pointer(opcode, arg(0), 0);
                }
                if results == 1 && res(0) != Type::PTR {
                    self.error(format!(
                        "va_object answers where the object is and {} is not an address",
                        res(0)
                    ));
                }
            }

            // Control.
            Opcode::Jump => {
                self.takes(opcode, arity, 0);
                self.targets(func, inst, 1);
            }
            Opcode::BrIf => {
                if self.takes(opcode, arity, 1) && arg(0) != Type::I1 {
                    self.error(format!("br_if branches on an i1 and this one on {}", arg(0)));
                }
                self.targets(func, inst, 2);
            }
            Opcode::Switch => {
                if self.takes(opcode, arity, 1) {
                    self.integer(opcode, arg(0), 0);
                }
            }
            Opcode::BlockAddr => {
                self.takes(opcode, arity, 0);
                self.targets(func, inst, 1);
                if results == 1 && !res(0).is_ptr() {
                    self.error(format!(
                        "block_addr produces a pointer and this one produces {}",
                        res(0)
                    ));
                }
                if func.successors(inst).any(|call| !func[call.args].is_empty()) {
                    // Taking the address is not arriving, so there is nothing to hand over.
                    // What the block takes is passed by the branch that goes there.
                    self.error("block_addr names a block and passes it arguments");
                }
            }
            Opcode::IndirectBr => {
                if self.takes(opcode, arity, 1) {
                    self.pointer(opcode, arg(0), 0);
                }
                // No count to check: how many blocks one of these can arrive at is how many
                // the front end says, and a `goto *p` in a function with one label has one.
            }
            Opcode::Return => {
                let want = &func.signature().returns;
                if arity != want.len() {
                    self.error(format!(
                        "the signature returns {} and this returns {arity}",
                        want.len()
                    ));
                } else {
                    for (index, ty) in want.iter().map(|param| param.ty).enumerate() {
                        if arg(index) != ty {
                            self.error(format!(
                                "result {} of the signature is {ty} and this returns {}",
                                index + 1,
                                arg(index)
                            ));
                        }
                    }
                }
            }

            // Calls, whose signature is what says the shape.
            Opcode::Call | Opcode::CallIndirect | Opcode::TailCall => {
                let Extra::Call(at) = data.extra else { return };
                let info = func[at];
                let signature = &func[info.signature];
                let indirect = usize::from(opcode == Opcode::CallIndirect);
                if indirect == 1 {
                    if arity == 0 {
                        self.error("call_indirect calls through a pointer and has no operands");
                        return;
                    }
                    self.pointer(opcode, arg(0), 0);
                }
                let passed = arity - indirect;
                let enough = if signature.variadic {
                    passed >= signature.params.len()
                } else {
                    passed == signature.params.len()
                };
                if enough {
                    for (index, ty) in signature.param_types().enumerate() {
                        if arg(index + indirect) != ty {
                            self.error(format!(
                                "parameter {} of the signature is {ty} and this argument is {}",
                                index + 1,
                                arg(index + indirect)
                            ));
                        }
                    }
                } else {
                    self.error(format!(
                        "the signature takes {}{} and this call passes {passed}",
                        signature.params.len(),
                        if signature.variadic { " or more" } else { "" }
                    ));
                }
                self.varargs(func, &info, passed, |n| arg(n + indirect));
                if results != signature.returns.len() {
                    self.error(format!(
                        "the signature returns {} and this call produces {results}",
                        signature.returns.len()
                    ));
                } else {
                    for (index, ty) in signature.return_types().enumerate() {
                        if res(index) != ty {
                            self.error(format!(
                                "result {} of the signature is {ty} and this call produces {}",
                                index + 1,
                                res(index)
                            ));
                        }
                    }
                }
                // Where the callee is in this module, the signature at the call and the one at
                // the function are the same signature or one of them is wrong.
                if let Some(callee) = info.callee {
                    if let Some(SymbolRef::Func(id)) = self.module.lookup(callee) {
                        if self.module[id].signature() != signature {
                            self.error(format!(
                                "@{} is declared here with another signature",
                                self.names.resolve(callee)
                            ));
                        }
                    }
                }
            }

            // Bit counting, which answers in the type it was asked about.
            Opcode::Ctlz | Opcode::Cttz | Opcode::Ctpop | Opcode::Bswap | Opcode::Bitreverse => {
                if self.takes(opcode, arity, 1) {
                    self.integer(opcode, arg(0), 0);
                    if results == 1 {
                        self.produces(opcode, res(0), arg(0));
                    }
                }
            }

            // The overflow-checked forms, which answer the value and whether it wrapped.
            Opcode::SAddOverflow
            | Opcode::UAddOverflow
            | Opcode::SSubOverflow
            | Opcode::USubOverflow
            | Opcode::SMulOverflow
            | Opcode::UMulOverflow => {
                if self.takes(opcode, arity, 2) {
                    self.integer(opcode, arg(0), 0);
                    self.agree(opcode, arg(0), arg(1));
                    if results == 2 {
                        self.produces(opcode, res(0), arg(0));
                        self.produces(opcode, res(1), arg(0).with_lane(Type::I1));
                    }
                }
            }
            Opcode::Expect => {
                if self.takes(opcode, arity, 2) {
                    self.agree(opcode, arg(0), arg(1));
                    if results == 1 {
                        self.produces(opcode, res(0), arg(0));
                    }
                }
            }

            // What is left says nothing about its operands here: the two markers are placed by
            // the front end around code the optimizer must not move, inline assembly is
            // whatever its constraints say, and a target intrinsic is the target's own rule.
            Opcode::SetjmpMarker
            | Opcode::LongjmpMarker
            | Opcode::InlineAsm
            | Opcode::TargetIntrinsic => {}
        }
    }

    // The small questions the shapes are made of.

    /// Reports the operand count when it is wrong, and answers whether it was right.
    fn takes(&mut self, opcode: Opcode, got: usize, want: usize) -> bool {
        if got == want {
            return true;
        }
        self.error(format!("{} takes {want} operands and this one has {got}", opcode.name()));
        false
    }

    /// Reports the number of branch targets when it is wrong.
    fn targets(&mut self, func: &'a Func, inst: Inst, want: usize) {
        let got = func.successors(inst).count();
        if got != want {
            self.error(format!(
                "{} branches to {want} blocks and this one to {got}",
                func[inst].opcode.name()
            ));
        }
    }

    fn integer(&mut self, opcode: Opcode, ty: Type, n: usize) {
        if !ty.lane().is_int() {
            self.error(format!(
                "operand {} of {} is an integer and this one is {ty}",
                n + 1,
                opcode.name()
            ));
        }
    }

    fn floating(&mut self, opcode: Opcode, ty: Type, n: usize) {
        if !ty.lane().is_float() {
            self.error(format!(
                "operand {} of {} is a floating point value and this one is {ty}",
                n + 1,
                opcode.name()
            ));
        }
    }

    fn pointer(&mut self, opcode: Opcode, ty: Type, n: usize) {
        if !ty.is_ptr() {
            self.error(format!(
                "operand {} of {} is a pointer and this one is {ty}",
                n + 1,
                opcode.name()
            ));
        }
    }

    /// Two operands that have to be the same type as each other.
    fn agree(&mut self, opcode: Opcode, first: Type, second: Type) {
        if first != second {
            self.error(format!(
                "the operands of {} have one type and these are {first} and {second}",
                opcode.name()
            ));
        }
    }

    /// A result that has to be a particular type.
    fn produces(&mut self, opcode: Opcode, got: Type, want: Type) {
        if got != want {
            self.error(format!(
                "{} produces {want} here and this one produces {got}",
                opcode.name()
            ));
        }
    }

    /// A conversion that keeps the lane count, since none of them changes it.
    fn lanes(&mut self, opcode: Opcode, to: Type, from: Type) {
        if to.lanes() != from.lanes() {
            self.error(format!(
                "{} keeps the lane count and {from} has {} and {to} has {}",
                opcode.name(),
                from.lanes(),
                to.lanes()
            ));
        }
    }

    /// A conversion that has to go the way its name says.
    fn widens(&mut self, opcode: Opcode, to: Type, from: Type, wider: bool) {
        let (a, b) = (to.lane().bits(), from.lane().bits());
        let ok = if wider { a > b } else { a < b };
        if !ok {
            let way = if wider { "wider" } else { "narrower" };
            self.error(format!(
                "{} produces something {way} and {from} to {to} is not",
                opcode.name()
            ));
        }
    }

    // Reporting.

    fn error(&mut self, message: impl Into<String>) {
        let at = self.locate();
        self.at(at, message);
    }

    fn at(&mut self, at: String, message: impl Into<String>) {
        self.errors.push(VerifyError { at, message: message.into() });
    }

    /// Where the walk is, as text.
    ///
    /// A block is named by its index rather than by the number the printer would give it,
    /// which is the same number for a module that came from the parser and can differ for one
    /// a pass has been reordering.
    fn locate(&self) -> String {
        use fmt::Write as _;
        let mut at = String::new();
        if let Some(func) = self.func {
            let _ = write!(at, "@{}", self.names.resolve(func.name));
            if let Some(block) = self.block {
                let _ = write!(at, " block{}", block.raw());
            }
            if let Some(inst) = self.inst {
                let _ = write!(at, " {}", func[inst].opcode.name());
            }
        }
        at
    }
}

/// Where each instruction is, so that a use in the same block as its definition can be told
/// from a use before it.
struct Layout {
    block: Vec<Option<Block>>,
    position: Vec<u32>,
}

impl Layout {
    fn new(func: &Func) -> Self {
        let counts = func.counts();
        let mut layout =
            Layout { block: vec![None; counts.insts], position: vec![0; counts.insts] };
        for block in func.blocks() {
            for (position, inst) in func.insts(block).enumerate() {
                layout.block[inst.index()] = Some(block);
                layout.position[inst.index()] = position as u32;
            }
        }
        layout
    }

    fn block_of(&self, inst: Inst) -> Option<Block> {
        self.block[inst.index()]
    }

    fn position(&self, inst: Inst) -> u32 {
        self.position[inst.index()]
    }
}

/// Which block dominates which, by the iterative algorithm of Cooper, Harvey and Kennedy,
/// "A Simple, Fast Dominance Algorithm" (2001).
///
/// The one in the verifier rather than a shared analysis, because the optimizer's dominator
/// tree is incrementally maintained across a pass and this one is built from nothing every time
/// it is asked for. The two want different things from the same idea.
struct Doms {
    /// Where each block is in reverse postorder, which is the order the fixed point converges
    /// fastest in, and `None` for one the entry does not reach.
    rank: Vec<Option<u32>>,
    /// The rank of each block's immediate dominator, indexed by rank.
    idom: Vec<u32>,
}

impl Doms {
    fn new(func: &Func) -> Self {
        let counts = func.counts();
        let entry = func.entry().expect("a function with blocks has a first one");

        // Every instruction that names a block, and not only the terminator. The one other
        // instruction that names one is `block_addr`, whose block is somewhere an
        // `indirect_br` can arrive at from anywhere the address reaches, so counting it as an
        // edge is what keeps a label that is only jumped to indirectly out of the reachability
        // report. The edge is a real one in the only direction that matters here: it can add
        // predecessors to a block and so take dominators away from it, which makes the check
        // on the uses stricter and never looser.
        let mut succs: Vec<Vec<Block>> = vec![Vec::new(); counts.blocks];
        for block in func.blocks() {
            for inst in func.insts(block) {
                succs[block.index()].extend(func.successors(inst).map(|call| call.block));
            }
        }

        // Postorder by an explicit stack, since a chain of blocks can be as long as the
        // function is and the recursive form would run out of stack on one.
        let mut order = Vec::new();
        let mut seen = vec![false; counts.blocks];
        let mut stack = vec![(entry, 0usize)];
        seen[entry.index()] = true;
        while let Some((block, next)) = stack.pop() {
            match succs[block.index()].get(next) {
                Some(&target) => {
                    stack.push((block, next + 1));
                    if !seen[target.index()] {
                        seen[target.index()] = true;
                        stack.push((target, 0));
                    }
                }
                None => order.push(block),
            }
        }
        order.reverse();

        let mut rank = vec![None; counts.blocks];
        for (index, &block) in order.iter().enumerate() {
            rank[block.index()] = Some(index as u32);
        }
        let mut preds: Vec<Vec<u32>> = vec![Vec::new(); order.len()];
        for (index, &block) in order.iter().enumerate() {
            for &target in &succs[block.index()] {
                if let Some(target) = rank[target.index()] {
                    preds[target as usize].push(index as u32);
                }
            }
        }

        // The entry dominates itself, and everything else starts undefined, which is what the
        // sentinel is. The fixed point is reached in one pass over a reducible CFG and in two
        // over the ones a `goto` produces.
        const NONE: u32 = u32::MAX;
        let mut idom = vec![NONE; order.len()];
        if !order.is_empty() {
            idom[0] = 0;
        }
        let mut changed = true;
        while changed {
            changed = false;
            for index in 1..order.len() {
                let mut new = NONE;
                for &pred in &preds[index] {
                    if idom[pred as usize] == NONE {
                        continue;
                    }
                    new = if new == NONE { pred } else { meet(&idom, new, pred) };
                }
                if new != NONE && idom[index] != new {
                    idom[index] = new;
                    changed = true;
                }
            }
        }
        Doms { rank, idom }
    }

    /// Whether the entry block reaches this one at all.
    fn reaches(&self, block: Block) -> bool {
        self.rank[block.index()].is_some()
    }

    /// Whether every path from the entry to `block` goes through `of`.
    ///
    /// A block the entry does not reach is dominated by everything, which is vacuously true and
    /// keeps an unreachable block from being reported twice over, once for being unreachable
    /// and once for every value it uses.
    fn dominates(&self, of: Block, block: Block) -> bool {
        let (Some(a), Some(b)) = (self.rank[of.index()], self.rank[block.index()]) else {
            return true;
        };
        let mut walk = b;
        while walk > a {
            walk = self.idom[walk as usize];
        }
        walk == a
    }
}

/// The nearest block that dominates both, walking the two chains towards the entry.
fn meet(idom: &[u32], mut a: u32, mut b: u32) -> u32 {
    while a != b {
        while a > b {
            a = idom[a as usize];
        }
        while b > a {
            b = idom[b as usize];
        }
    }
    a
}

/// How many bits a value of that type occupies, counting every lane.
fn width(ty: Type) -> u64 {
    u64::from(ty.bits()) * u64::from(ty.lanes())
}

/// A list of types, as the text writes them.
fn types(list: &[Type]) -> String {
    if list.is_empty() {
        return "nothing".to_string();
    }
    list.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use rucc_base::{Idx, Interner, Symbol};
    use rucc_diag::Span;
    use rucc_target::{Arch, Env, Os, Triple};

    use super::*;
    use crate::fixtures::{EXAMPLE, SYMBOLS, ZOO};
    use crate::func::Builder;
    use crate::inst::{InstData, MetaNode, Signature};
    use crate::{Flags, IntPred, parse};

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// Everything wrong with a module written as text, which is how most of these are written:
    /// the parser turns down what is malformed and what is left is what the verifier is for.
    fn errors(text: &str) -> Vec<String> {
        let mut names = Interner::new();
        let module = match parse(text, &mut names) {
            Ok(module) => module,
            Err(error) => panic!("{error}"),
        };
        match verify(&module, &names) {
            Ok(()) => Vec::new(),
            Err(errors) => errors.iter().map(ToString::to_string).collect(),
        }
    }

    /// The one thing wrong with it.
    fn only(text: &str) -> String {
        let found = errors(text);
        assert_eq!(found.len(), 1, "{found:#?}");
        found.into_iter().next().expect("just counted one")
    }

    const HEADER: &str = "\
; ModuleID = 'bad.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"
";

    /// A function around that body, with that signature.
    fn wrap(signature: &str, body: &str) -> String {
        format!("{HEADER}\nfunc @f{signature}, linkage(external) {{\n{body}}}\n")
    }

    #[test]
    fn the_three_fixtures_are_modules_the_compiler_may_believe() {
        for text in [EXAMPLE, ZOO, SYMBOLS] {
            assert_eq!(errors(text), Vec::<String>::new());
        }
    }

    #[test]
    fn a_use_that_its_definition_does_not_reach_is_reported() {
        let text = wrap(
            "(i1) -> i32",
            "block0(%0: i1):
    br_if %0, block1, block2

block1:
    %1 = iconst.i32 7
    jump block2

block2:
    return %1
",
        );
        assert_eq!(
            only(&text),
            "@f block2 return: %1 is produced in block1 and does not reach here"
        );
    }

    #[test]
    fn a_use_before_its_definition_in_the_same_block_is_reported() {
        let text = wrap(
            "() -> i32",
            "block0:
    %0 = iconst.i32 1
    %1 = add %2, %0
    %2 = iconst.i32 2
    return %1
",
        );
        assert_eq!(only(&text), "@f block0 add: %2 is produced in block0 and does not reach here");
    }

    #[test]
    fn a_call_that_takes_its_arguments_the_way_the_abi_says_is_believed() {
        let text = wrap(
            "(ptr sret(24, align 8), ptr byval(16, align 8), i8 zext)",
            "block0(%0: ptr, %1: ptr, %2: i8):
    return
",
        );
        assert_eq!(errors(&text), Vec::<String>::new());
    }

    #[test]
    fn a_call_that_says_how_an_argument_past_its_parameter_list_travels_is_believed() {
        let text = wrap(
            "(ptr)",
            "block0(%0: ptr):
    call @p(%0, %0 byval(24, align 8)) : (ptr, ...)
    return
",
        );
        assert_eq!(errors(&text), Vec::<String>::new());
    }

    #[test]
    fn a_call_that_says_it_of_something_that_is_not_a_pointer_is_reported() {
        let text = wrap(
            "(ptr)",
            "block0(%0: ptr):
    %1 = iconst.i32 1
    call @p(%0, %1 byval(4, align 4)) : (ptr, ...)
    return
",
        );
        assert_eq!(
            only(&text),
            "@f block0 call: argument 2 travels indirectly and i32 is not a pointer"
        );
    }

    #[test]
    fn a_call_that_says_an_argument_past_its_parameter_list_is_an_sret_is_reported() {
        let text = wrap(
            "(ptr)",
            "block0(%0: ptr):
    call @p(%0, %0 sret(24, align 8)) : (ptr, ...)
    return
",
        );
        assert_eq!(
            only(&text),
            "@f block0 call: argument 2 is an sret and only a parameter can be one"
        );
    }

    #[test]
    fn a_call_that_is_not_variadic_says_nothing_about_a_variadic_argument() {
        let text = wrap(
            "(ptr)",
            "block0(%0: ptr):
    call @p(%0, %0 byval(24, align 8)) : (ptr)
    return
",
        );
        assert_eq!(
            errors(&text),
            vec![
                "@f block0 call: the signature takes 1 and this call passes 2",
                "@f block0 call: this call is not variadic and says how a variadic argument \
                 travels",
            ]
        );
    }

    #[test]
    fn an_sret_that_is_not_the_first_parameter_is_reported() {
        let text = wrap(
            "(ptr byval(8, align 8), ptr sret(16, align 8))",
            "block0(%0: ptr, %1: ptr):
    return
",
        );
        assert_eq!(only(&text), "@f: sret is the first parameter and this one is not");
    }

    #[test]
    fn a_function_returning_through_sret_returns_nothing_else() {
        let text = wrap(
            "(ptr sret(8, align 8)) -> i32",
            "block0(%0: ptr):
    %1 = iconst.i32 7
    return %1
",
        );
        assert_eq!(only(&text), "@f: a signature returning through sret returns nothing else");
    }

    #[test]
    fn an_object_that_travels_indirectly_travels_behind_a_pointer() {
        let text = wrap(
            "(i32 byval(4, align 4))",
            "block0(%0: i32):
    return
",
        );
        assert_eq!(only(&text), "@f: parameter 1 travels indirectly and i32 is not a pointer");
    }

    #[test]
    fn an_alignment_a_parameter_could_not_have_is_reported() {
        let text = wrap(
            "(ptr byval(24, align 3))",
            "block0(%0: ptr):
    return
",
        );
        assert_eq!(only(&text), "@f: an alignment is a power of two and this is 3");
    }

    /// The slots say which bytes of the object came out of which register, so one reaching past
    /// the end of the object is a classification that belongs to some other object, and a backend
    /// acting on it would copy memory this object never had.
    #[test]
    fn a_slot_holding_bytes_the_object_does_not_have_is_reported() {
        let text = wrap(
            "(ptr) -> ptr",
            "block0(%0: ptr):
    %1 = va_object %0, size 12, align 8, in(int 8 at 0, int 8 at 8)
    return %1
",
        );
        assert_eq!(
            only(&text),
            "@f block0 va_object: a slot holds bytes 8 to 16 of an object of 12 bytes"
        );
    }

    #[test]
    fn a_result_does_not_travel_indirectly() {
        // A return value too large for the registers comes back through a parameter, so this
        // says an ABI nothing implements.
        let text = wrap(
            "() -> ptr byval(16, align 8)",
            "block0:
    %0 = iconst.i64 0
    %1 = inttoptr.ptr %0
    return %1
",
        );
        assert_eq!(only(&text), "@f: result 1 travels indirectly and a result cannot");
    }

    #[test]
    fn an_extension_is_asked_of_an_integer_and_not_of_anything_else() {
        let text = wrap(
            "(ptr zext)",
            "block0(%0: ptr):
    return
",
        );
        assert_eq!(only(&text), "@f: parameter 1 is extended and ptr is not an integer");
    }

    #[test]
    fn a_branch_that_passes_the_wrong_number_of_arguments_is_reported() {
        let text = wrap(
            "(i32)",
            "block0(%0: i32):
    jump block1(%0)

block1:
    return
",
        );
        assert_eq!(
            only(&text),
            "@f block0 jump: block1 takes 0 arguments and this branch passes 1"
        );
    }

    #[test]
    fn a_branch_that_passes_the_wrong_type_is_reported() {
        let text = wrap(
            "(i32) -> i32",
            "block0(%0: i32):
    %1 = sext.i64 %0
    jump block1(%1)

block1(%2: i32):
    return %2
",
        );
        assert_eq!(only(&text), "@f block0 jump: argument 1 to block1 is i32 and this one is i64");
    }

    #[test]
    fn a_block_that_does_not_end_in_a_terminator_is_reported() {
        let text = wrap("()", "block0:\n    %0 = iconst.i32 1\n");
        assert_eq!(only(&text), "@f block0: this block does not end in a terminator");
    }

    #[test]
    fn an_instruction_after_the_terminator_is_reported() {
        let text = wrap("()", "block0:\n    return\n    %0 = iconst.i32 1\n");
        assert_eq!(only(&text), "@f block0 iconst: this comes after the block's terminator");
    }

    #[test]
    fn an_unreachable_block_is_reported() {
        let text = wrap("()", "block0:\n    return\n\nblock1:\n    return\n");
        assert_eq!(only(&text), "@f block1: this block is not reachable and has not been deleted");
    }

    #[test]
    fn a_block_a_jump_to_an_address_arrives_at_is_an_ordinary_target() {
        let text = wrap(
            "(ptr) -> i32",
            "block0(%0: ptr):
    %1 = block_addr block1
    indirect_br %0, block1

block1:
    %2 = iconst.i32 1
    return %2
",
        );
        assert_eq!(errors(&text), Vec::<String>::new());
    }

    #[test]
    fn a_block_whose_address_is_taken_is_reached_by_the_taking_of_it() {
        // Nothing branches to block1 here and its address has left the function, so the one
        // thing that says it is still somewhere control can arrive at is the `block_addr`.
        let text = wrap(
            "() -> ptr",
            "block0:
    %0 = block_addr block1
    return %0

block1:
    unreachable
",
        );
        assert_eq!(errors(&text), Vec::<String>::new());
    }

    #[test]
    fn taking_the_address_of_a_block_and_passing_it_arguments_is_reported() {
        // Taking the address is not arriving, so there is nothing to hand over. What block1
        // takes is passed by whatever branches there.
        let text = wrap(
            "(i32) -> ptr",
            "block0(%0: i32):
    %1 = block_addr block1(%0)
    return %1

block1(%2: i32):
    unreachable
",
        );
        assert_eq!(
            only(&text),
            "@f block0 block_addr: block_addr names a block and passes it arguments"
        );
    }

    #[test]
    fn a_jump_to_something_that_is_not_an_address_is_reported() {
        let text = wrap(
            "(i32)",
            "block0(%0: i32):
    indirect_br %0, block1

block1:
    return
",
        );
        assert_eq!(
            only(&text),
            "@f block0 indirect_br: operand 1 of indirect_br is a pointer and this one is i32"
        );
    }

    #[test]
    fn a_branch_back_to_the_entry_block_is_reported() {
        // The entry block's parameters are the function's arguments, so a branch to it would be
        // a second place they arrive from.
        let text = wrap(
            "(i32)",
            "block0(%0: i32):
    jump block1

block1:
    jump block0(%0)
",
        );
        assert_eq!(
            only(&text),
            "@f block0: the entry block is branched to, and it takes the arguments"
        );
    }

    #[test]
    fn an_entry_block_that_does_not_take_the_arguments_is_reported() {
        let text = wrap("(i32)", "block0(%0: i64):\n    return\n");
        assert_eq!(only(&text), "@f: the entry block takes i64 and the signature says i32");
    }

    #[test]
    fn a_flag_the_opcode_does_not_read_is_reported() {
        let text =
            wrap("(i32) -> i32", "block0(%0: i32):\n    %1 = add.exact %0, %0\n    return %1\n");
        assert_eq!(only(&text), "@f block0 add: add does not read `exact`");
    }

    #[test]
    fn an_ordering_the_operation_cannot_be_asked_for_is_reported() {
        let text = wrap(
            "(ptr) -> i32",
            "block0(%0: ptr):\n    %1 = atomic_load.i32 %0, align 4, release\n    return %1\n",
        );
        assert_eq!(only(&text), "@f block0 atomic_load: atomic_load cannot be asked for release");
    }

    #[test]
    fn an_ordering_on_the_non_atomic_form_is_reported() {
        let text = wrap(
            "(ptr) -> i32",
            "block0(%0: ptr):\n    %1 = load.i32 %0, align 4, acquire\n    return %1\n",
        );
        assert_eq!(only(&text), "@f block0 load: load cannot be asked for acquire");
    }

    #[test]
    fn a_va_object_that_answers_anything_but_an_address_is_reported() {
        // Where the object is, which is the only thing it can answer, since the object it reads
        // is an aggregate and an aggregate is not a value the IR has a type for.
        let text = wrap(
            "(ptr) -> i64",
            "block0(%0: ptr):
    %1 = va_object.i64 %0, size 16, align 8
    return %1
",
        );
        assert_eq!(
            only(&text),
            "@f block0 va_object: va_object answers where the object is and i64 is not an address"
        );
    }

    #[test]
    fn an_alloca_of_a_fixed_size_outside_the_entry_block_is_reported() {
        let text = wrap(
            "()",
            "block0:
    jump block1

block1:
    %0 = alloca, size 16, align 8
    return
",
        );
        assert_eq!(
            only(&text),
            "@f block1 alloca: an alloca of a fixed size belongs in the entry block"
        );
    }

    #[test]
    fn a_dynamic_alloca_may_be_anywhere() {
        let text = wrap(
            "(i64)",
            "block0(%0: i64):
    jump block1

block1:
    %1 = alloca %0, align 8
    return
",
        );
        assert_eq!(errors(&text), Vec::<String>::new());
    }

    #[test]
    fn two_cases_of_a_switch_with_the_same_value_are_reported() {
        let text = wrap(
            "(i32)",
            "block0(%0: i32):
    switch %0, block1, [7 => block1, 7 => block1]

block1:
    return
",
        );
        assert_eq!(only(&text), "@f block0 switch: two cases of this switch have the same value");
    }

    #[test]
    fn operands_that_do_not_agree_are_reported() {
        let text = wrap(
            "(i32, i64) -> i32",
            "block0(%0: i32, %1: i64):\n    %2 = add %0, %1\n    return %2\n",
        );
        assert_eq!(
            only(&text),
            "@f block0 add: the operands of add have one type and these are i32 and i64"
        );
    }

    #[test]
    fn a_conversion_that_goes_the_wrong_way_is_reported() {
        let text = wrap("(i32) -> i64", "block0(%0: i32):\n    %1 = trunc.i64 %0\n    return %1\n");
        assert_eq!(
            only(&text),
            "@f block0 trunc: trunc produces something narrower and i32 to i64 is not"
        );
    }

    #[test]
    fn a_bitcast_between_an_address_and_a_number_is_reported() {
        let text =
            wrap("(ptr) -> i64", "block0(%0: ptr):\n    %1 = bitcast.i64 %0\n    return %1\n");
        assert_eq!(
            only(&text),
            "@f block0 bitcast: a bitcast between a pointer and a number is ptrtoint or inttoptr"
        );
    }

    #[test]
    fn an_operand_of_the_wrong_kind_is_reported() {
        let text = wrap("(i32) -> i32", "block0(%0: i32):\n    %1 = fadd %0, %0\n    return %1\n");
        assert_eq!(
            only(&text),
            "@f block0 fadd: operand 1 of fadd is a floating point value and this one is i32"
        );
    }

    #[test]
    fn a_condition_that_is_not_one_bit_is_reported() {
        let text = wrap(
            "(i32)",
            "block0(%0: i32):
    br_if %0, block1, block1

block1:
    return
",
        );
        assert_eq!(only(&text), "@f block0 br_if: br_if branches on an i1 and this one on i32");
    }

    #[test]
    fn a_return_that_does_not_match_the_signature_is_reported() {
        let text = wrap("() -> i32", "block0:\n    return\n");
        assert_eq!(only(&text), "@f block0 return: the signature returns 1 and this returns 0");
    }

    #[test]
    fn a_call_that_disagrees_with_the_declaration_is_reported() {
        let text = format!(
            "{HEADER}
func @g(i32, ...) -> i32, linkage(external);

func @f(i32) -> i32, linkage(external) {{
block0(%0: i32):
    %1 = call @g(%0) : (i32) -> i32
    return %1
}}
"
        );
        assert_eq!(only(&text), "@f block0 call: @g is declared here with another signature");
    }

    #[test]
    fn a_global_whose_image_is_not_its_size_is_reported() {
        let text =
            format!("{HEADER}\nglobal @x : bytes 8 = {{ i32 7 }}, align 4, linkage(external)\n");
        assert_eq!(only(&text), "@x: the image is 4 bytes and the global is 8");
    }

    #[test]
    fn a_pointer_in_an_image_has_no_width_and_is_reported() {
        // What a `NULL` in a static initializer used to become. A `ptr` takes the width of the
        // target's addresses and a type says nothing about the target, so the datum measured
        // zero bytes and the value it held went nowhere.
        let text = format!(
            "{HEADER}\nglobal @x : bytes 8 = {{ ptr 0x0, zero 8 }}, align 8, linkage(external)\n"
        );
        assert_eq!(only(&text), "@x: a scalar in an image has a width and ptr has none");
    }

    #[test]
    fn a_declaration_of_something_another_module_defines_may_be_constant() {
        // `extern const int x;` names an object in the library's read only data. Whether it may
        // be written through is a fact about the object rather than about who holds the bytes.
        let text = format!("{HEADER}\nglobal @x : bytes 4, align 4, linkage(external), constant\n");
        assert!(errors(&text).is_empty(), "{:?}", errors(&text));
    }

    #[test]
    fn an_alias_to_itself_is_reported() {
        let text = format!("{HEADER}\nalias @a = @a, linkage(external)\n");
        assert_eq!(only(&text), "@a: an alias to itself");
    }

    #[test]
    fn an_ifunc_that_does_not_resolve_through_a_function_is_reported() {
        let text = format!(
            "{HEADER}
global @g : i32 = 0, align 4, linkage(external)

ifunc @f = @g, linkage(external)
"
        );
        assert_eq!(
            only(&text),
            "@f: an ifunc resolves through a function and this target is not one"
        );
    }

    #[test]
    fn attributes_that_contradict_each_other_are_reported() {
        let text =
            format!("{HEADER}\nfunc @f(), linkage(external), attrs(always_inline, noinline);\n");
        assert_eq!(
            only(&text),
            "@f: `always_inline` and `noinline` cannot both be true of a function"
        );
    }

    // The rest are things no text can say, because the parser turns them down before the
    // verifier would see them. They are what a pass produces, which is who the verifier is for.

    fn one_error(module: &Module, func: &Func, names: &Interner) -> String {
        match verify_func(module, func, names) {
            Ok(()) => panic!("that was expected to be turned down"),
            Err(errors) => {
                assert_eq!(errors.len(), 1, "{errors:#?}");
                errors[0].to_string()
            }
        }
    }

    #[test]
    fn a_value_the_function_does_not_have_is_reported() {
        let mut names = Interner::new();
        let module = Module::new(names.intern("built.c"), &target());
        let mut func = Func::new(names.intern("f"), Signature::new());
        let block = func.create_block();
        let args = func.push_values(&[Value::from_usize(9)]);
        let inst =
            func.create_inst(InstData { args, ..InstData::new(Opcode::Return) }, &[], Span::DUMMY);
        func.append_inst(block, inst);
        assert_eq!(
            one_error(&module, &func, &names),
            "@f block0 return: %9 is not a value of this function"
        );
    }

    #[test]
    fn a_value_whose_definition_has_been_taken_out_is_reported() {
        let mut names = Interner::new();
        let module = Module::new(names.intern("built.c"), &target());
        let i32_ = Type::int(32);
        let mut func = Func::new(names.intern("f"), Signature::new().with_returns(&[i32_]));
        let block = func.create_block();
        let mut b = Builder::new(&mut func, block);
        let value = b.iconst(i32_, 7);
        b.ret(&[value]);
        let Def::Result { inst, .. } = func[value].def else { unreachable!("a constant") };
        func.remove_inst(inst);
        assert_eq!(
            one_error(&module, &func, &names),
            "@f block0 return: %0 is produced by an instruction that is not in the function"
        );
    }

    #[test]
    fn an_instruction_carrying_another_opcodes_payload_is_reported() {
        // This one prints as text the parser cannot read back, so the round trip would fail on
        // it somewhere else entirely if the verifier did not say so here.
        let mut names = Interner::new();
        let module = Module::new(names.intern("built.c"), &target());
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[Type::int(32)]));
        let block = func.create_block();
        let param = func.append_param(block, Type::int(32));
        let args = func.push_values(&[param, param]);
        let inst = func.create_inst(
            InstData { args, extra: Extra::IntPred(IntPred::Eq), ..InstData::new(Opcode::Add) },
            &[Type::int(32)],
            Span::DUMMY,
        );
        func.append_inst(block, inst);
        let ret = func.create_inst(InstData::new(Opcode::Return), &[], Span::DUMMY);
        func.append_inst(block, ret);
        assert_eq!(
            one_error(&module, &func, &names),
            "@f block0 add: add carries nothing and this one carries an integer comparison"
        );
    }

    #[test]
    fn a_metadata_node_that_is_its_own_parent_is_reported() {
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("built.c"), &target());
        module.add_meta(MetaNode {
            name: names.intern("int"),
            parent: Some(Idx::from_usize(0)),
            offset: 0,
        });
        let found = match verify(&module, &names) {
            Ok(()) => panic!("that was expected to be turned down"),
            Err(errors) => errors,
        };
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(
            found[0].to_string(),
            "!0: a metadata node's parent comes before it and this one is !0"
        );
    }

    #[test]
    fn a_datalayout_the_target_does_not_imply_is_reported() {
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("built.c"), &target());
        module.datalayout = DataLayout::parse("e-p:32:32-i64:64-f80:32-S64").expect("a layout");
        let found = match verify(&module, &names) {
            Ok(()) => panic!("that was expected to be turned down"),
            Err(errors) => errors,
        };
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].to_string().starts_with("@built.c: the datalayout is "), "{}", found[0]);
    }

    #[test]
    fn a_flag_riding_along_where_it_is_read_is_not_reported() {
        let mut names = Interner::new();
        let module = Module::new(names.intern("built.c"), &target());
        let i32_ = Type::int(32);
        let mut func = Func::new(
            names.intern("f"),
            Signature::new().with_params(&[i32_]).with_returns(&[i32_]),
        );
        let block = func.create_block();
        let param = func.append_param(block, i32_);
        let mut b = Builder::new(&mut func, block);
        let sum = b.binary(Opcode::Add, param, param, Flags::NSW);
        b.ret(&[sum]);
        assert!(verify_func(&module, &func, &names).is_ok());
    }

    #[test]
    fn a_declaration_is_checked_and_has_nothing_else_to_check() {
        let mut names = Interner::new();
        let module = Module::new(names.intern("built.c"), &target());
        let func = Func::new(Symbol::from_raw(0), Signature::new());
        assert!(func.is_declaration());
        assert!(verify_func(&module, &func, &names).is_ok());
    }
}
