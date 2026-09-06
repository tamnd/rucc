//! The printer: a module as text.
//!
//! Design: `spec/08-ir.md` section 8.8.
//!
//! The printer and the parser round-trip byte for byte, which is what makes the IR testable on
//! its own, what makes `-fdump-ir=after-<pass>` worth reading, and what lets a fuzzer make IR
//! directly rather than through the front end. The printer is written first because the parser
//! has to read what it writes.
//!
//! # What decides the text
//!
//! Nothing printed here is a fact about the tables the module happens to be in. Values are
//! numbered in the order they are printed rather than by their index, blocks likewise, and a
//! signature is written out at the call rather than as a number into a side table. So printing
//! a module, parsing it back and printing it again gives the same bytes even when the second
//! module's tables are laid out differently from the first's, which is the property that makes
//! the round trip worth testing at all.
//!
//! A type is written on an instruction only where it cannot be worked out from the operands:
//! on one that takes none, and on one whose result is a different type from its first operand.
//! Everything else would be a second copy of something already on the line above, and a second
//! copy is a thing that can disagree.
//!
//! Spans are not printed. Debug information has its own form and it is written in a later
//! milestone; the round trip is a claim about the text, not about the source locations behind
//! it.

use std::fmt::Write as _;

use rucc_base::{Interner, Symbol};
use rucc_target::Slot;

use crate::func::Func;
use crate::inst::{
    Abi, Block, BlockCall, Imm, Inst, InstData, MemInfo, Meta, Param, Signature, Value,
};
use crate::module::{Alias, Datum, Global, Module, Reloc};
use crate::{Extra, FORMAT_VERSION, Linkage, MemOrder, Opcode, Type, Visibility};

/// Where an instruction sits on the memory chain, which the printer writes apart from the rest.
#[derive(Clone, Copy)]
struct Chain {
    /// The version of memory it reads, which is its last operand.
    takes: Option<Value>,
    /// Whether it makes a new one, which is its last result.
    gives: bool,
}

/// The results with the version of memory taken off the end.
///
/// The value itself is written on the left with the others, since a reader chasing the chain has
/// to be able to see where a version was made. It is the type suffix this comes off, because the
/// type of a version of memory is always `mem` and writing that down says nothing.
fn without_mem(mut results: Vec<Value>, chain: Chain) -> Vec<Value> {
    if chain.gives {
        results.pop();
    }
    results
}

/// Whether the opcode says what it produces without a type having to be written down.
///
/// A comparison produces `i1`, one per lane of what it compared. The two that produce an
/// address produce an address. A call produces what its signature says, and the signature is
/// written out on the same line. Everything else either takes an operand of the type it
/// produces, in which case that operand says it, or has the type written after the opcode.
///
/// The printer and the parser share this, because a rule the two of them state separately is a
/// rule they will eventually state differently.
pub(crate) fn implied_result(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::ICmp
            | Opcode::FCmp
            | Opcode::GlobalAddr
            | Opcode::BlockAddr
            | Opcode::Alloca
            | Opcode::Call
            | Opcode::CallIndirect
            | Opcode::TailCall
            // The five that make a capability make a capability, whatever they were given.
            | Opcode::CapOf
            | Opcode::CapLoad
            | Opcode::CapNull
            | Opcode::CapNarrow
            | Opcode::CapRecover
    )
}

/// The whole module, as text.
#[must_use]
pub fn print(module: &Module, names: &Interner) -> String {
    let mut printer = Printer::new(module, names);
    printer.module();
    printer.finish()
}

/// One function of a module, as text, for a dump of a single function.
#[must_use]
pub fn print_func(module: &Module, func: &Func, names: &Interner) -> String {
    let mut printer = Printer::new(module, names);
    printer.func(func);
    printer.finish()
}

/// A module being written out.
#[derive(Debug)]
pub struct Printer<'a> {
    module: &'a Module,
    names: &'a Interner,
    out: String,
    // The number each value and each block is printed as, in print order, indexed by the index
    // it has in the function being printed. `u32::MAX` for one that has not been reached,
    // which only happens in a function the verifier would turn down.
    values: Vec<u32>,
    blocks: Vec<u32>,
}

impl<'a> Printer<'a> {
    /// A printer over one module, whose names are in `names`.
    #[must_use]
    pub fn new(module: &'a Module, names: &'a Interner) -> Printer<'a> {
        Printer { module, names, out: String::new(), values: Vec::new(), blocks: Vec::new() }
    }

    /// The text written so far.
    #[must_use]
    pub fn finish(self) -> String {
        self.out
    }

    /// The header, then the globals, the aliases, the functions and the metadata.
    pub fn module(&mut self) {
        let module = self.module;
        let name = self.names.resolve(module.name);
        // Writing to a `String` cannot fail, which is why the result is dropped here and at
        // every other `write!` in this file rather than turned into a panic to reason about.
        let _ = writeln!(self.out, "; ModuleID = '{name}'");
        let _ = writeln!(self.out, "; format {FORMAT_VERSION}");
        let _ = writeln!(self.out, "target triple = \"{}\"", module.triple);
        let _ = writeln!(self.out, "target datalayout = \"{}\"", module.datalayout);

        if module.globals().next().is_some() {
            self.out.push('\n');
            for id in module.globals() {
                self.global(&module[id]);
            }
        }
        if module.aliases().next().is_some() {
            self.out.push('\n');
            for id in module.aliases() {
                self.alias(&module[id]);
            }
        }
        for id in module.funcs() {
            self.out.push('\n');
            self.func(&module[id]);
        }
        if module.metadata().next().is_some() {
            self.out.push('\n');
            for meta in module.metadata() {
                self.meta_node(meta);
            }
        }
    }

    // Globals and aliases.

    /// One global variable, on one line.
    fn global(&mut self, global: &Global) {
        let _ = write!(self.out, "global @{} : ", self.names.resolve(global.name));
        match self.scalar_init(global) {
            // The shorthand for the common case, which is a global holding one number. It is
            // used only when the type accounts for the whole size, so that reading it back
            // gives the size again without its having been written down.
            Some((ty, imm)) => {
                let _ = write!(self.out, "{ty} = ");
                self.imm(imm, ty);
            }
            None => {
                let _ = write!(self.out, "bytes {}", global.size);
                if let Some(init) = global.init {
                    // An image with nothing in it is written `{}`, with no space inside, because
                    // the spaces in the other spelling are there to hold the pieces apart and an
                    // empty image has none to hold. A zero sized object is where this comes from:
                    // `char x[0] = { };` at file scope has an image and the image has no pieces,
                    // and the reader used to stop on the empty one because it asked for a piece
                    // before it looked for the brace.
                    let data = &self.module[init];
                    if data.is_empty() {
                        self.out.push_str(" = {}");
                    } else {
                        self.out.push_str(" = { ");
                        for (index, &datum) in data.iter().enumerate() {
                            if index > 0 {
                                self.out.push_str(", ");
                            }
                            self.datum(datum);
                        }
                        self.out.push_str(" }");
                    }
                }
            }
        }
        let _ = write!(self.out, ", align {}", global.align);
        self.linkage(global.linkage, global.visibility);
        if let Some(model) = global.tls {
            let _ = write!(self.out, ", tls({})", model.name());
        }
        if global.constant {
            self.out.push_str(", constant");
        }
        self.section(global.section);
        self.out.push('\n');
    }

    /// The type and the value of a global that holds exactly one scalar filling it.
    fn scalar_init(&self, global: &Global) -> Option<(Type, Imm)> {
        let init = global.init?;
        let [datum] = self.module[init] else { return None };
        let Datum::Scalar { ty, value } = datum else { return None };
        (datum.size(self.module) == global.size).then(|| (ty, self.module[value]))
    }

    /// One piece of a global's image.
    fn datum(&mut self, datum: Datum) {
        match datum {
            Datum::Zero(bytes) => {
                let _ = write!(self.out, "zero {bytes}");
            }
            Datum::Bytes(range) => {
                self.out.push_str("bytes ");
                let bytes = &self.module[range];
                self.string(bytes);
            }
            Datum::Scalar { ty, value } => {
                let _ = write!(self.out, "{ty} ");
                self.imm(self.module[value], ty);
            }
            Datum::Addr(reloc) => {
                let Reloc { symbol, addend, size } = self.module[reloc];
                let _ = write!(self.out, "addr.{size} @{}", self.names.resolve(symbol));
                match addend.signum() {
                    1 => {
                        let _ = write!(self.out, " + {addend}");
                    }
                    -1 => {
                        // Written as a subtraction rather than as a negative addend, because
                        // `+ -8` is a thing nobody reads twice the same way. `i64::MIN` has no
                        // positive counterpart, so it keeps the sign it came with.
                        let _ = match addend.checked_neg() {
                            Some(amount) => write!(self.out, " - {amount}"),
                            None => write!(self.out, " + {addend}"),
                        };
                    }
                    _ => {}
                }
            }
        }
    }

    /// One alias, on one line.
    fn alias(&mut self, alias: &Alias) {
        let _ = write!(
            self.out,
            "{} @{} = @{}",
            alias.kind.name(),
            self.names.resolve(alias.name),
            self.names.resolve(alias.target)
        );
        self.linkage(alias.linkage, alias.visibility);
        self.out.push('\n');
    }

    // Functions.

    /// One function: its signature, then its blocks, or a semicolon if it has none.
    pub fn func(&mut self, func: &Func) {
        self.number(func);
        let _ = write!(self.out, "func @{}", self.names.resolve(func.name));
        self.signature(func.signature());
        self.linkage(func.linkage, func.visibility);
        if !func.attrs.is_default() {
            let _ = write!(self.out, ", {}", func.attrs);
        }
        self.section(func.section);
        if func.is_declaration() {
            self.out.push_str(";\n");
            return;
        }
        self.out.push_str(" {\n");
        for (index, block) in func.blocks().enumerate() {
            if index > 0 {
                self.out.push('\n');
            }
            self.block(func, block);
        }
        self.out.push_str("}\n");
    }

    /// Gives every value and every block of a function the number it is printed as.
    ///
    /// In print order, which is what makes the text a fact about the function's shape rather
    /// than about which order its tables were filled in.
    fn number(&mut self, func: &Func) {
        let counts = func.counts();
        self.values.clear();
        self.values.resize(counts.values, u32::MAX);
        self.blocks.clear();
        self.blocks.resize(counts.blocks, u32::MAX);
        let mut next = 0;
        for (index, block) in func.blocks().enumerate() {
            self.blocks[block.index()] = index as u32;
            for &param in &func[block].params {
                self.values[param.index()] = next;
                next += 1;
            }
            for inst in func.insts(block) {
                for result in func[inst].results() {
                    self.values[result.index()] = next;
                    next += 1;
                }
            }
        }
    }

    /// The parameter and result types of a function or a call, with what the ABI asks of each.
    fn signature(&mut self, signature: &Signature) {
        self.out.push('(');
        for (index, param) in signature.params.iter().enumerate() {
            if index > 0 {
                self.out.push_str(", ");
            }
            self.param(param);
        }
        if signature.variadic {
            if !signature.params.is_empty() {
                self.out.push_str(", ");
            }
            self.out.push_str("...");
        }
        self.out.push(')');
        match signature.returns.as_slice() {
            [] => {}
            [param] => {
                self.out.push_str(" -> ");
                self.param(param);
            }
            params => {
                self.out.push_str(" -> (");
                for (index, param) in params.iter().enumerate() {
                    if index > 0 {
                        self.out.push_str(", ");
                    }
                    self.param(param);
                }
                self.out.push(')');
            }
        }
    }

    /// One parameter: its type, and what the ABI asks of it when that is anything.
    fn param(&mut self, param: &Param) {
        let _ = write!(self.out, "{}", param.ty);
        self.abi(param.abi);
    }

    /// What the ABI asks of a value, after whatever it is written on, and nothing at all when
    /// the answer is that it travels as itself.
    fn abi(&mut self, abi: Abi) {
        let _ = match abi {
            Abi::Plain => Ok(()),
            Abi::Sext => write!(self.out, " sext"),
            Abi::Zext => write!(self.out, " zext"),
            Abi::ByVal { size, align } => write!(self.out, " byval({size}, align {align})"),
            Abi::Sret { size, align } => write!(self.out, " sret({size}, align {align})"),
        };
    }

    /// One block: its label with its parameters, then its instructions.
    fn block(&mut self, func: &Func, block: Block) {
        let _ = write!(self.out, "block{}", self.blocks[block.index()]);
        let params = &func[block].params;
        if !params.is_empty() {
            self.out.push('(');
            for (index, &param) in params.iter().enumerate() {
                if index > 0 {
                    self.out.push_str(", ");
                }
                self.value(param);
                let _ = write!(self.out, ": {}", func[param].ty);
            }
            self.out.push(')');
        }
        self.out.push_str(":\n");
        for inst in func.insts(block) {
            self.inst(func, inst);
        }
    }

    /// One instruction, indented, on one line.
    fn inst(&mut self, func: &Func, inst: Inst) {
        let data = func[inst];
        // Where the function is on the memory chain, the version of memory it takes is the last
        // operand and the one it makes is the last result. Both are written apart from the rest,
        // at the end as `[mem %3]`, because the reader is nearly always following the values and
        // not the chain, and an operand list that grows by one on every load is in the way.
        let chain = Chain { takes: func.mem_in(inst), gives: func.mem_out(inst).is_some() };
        self.out.push_str("    ");
        for (index, result) in data.results().enumerate() {
            if index > 0 {
                self.out.push_str(", ");
            }
            self.value(result);
        }
        if data.results > 0 {
            self.out.push_str(" = ");
        }
        self.out.push_str(data.opcode.name());
        self.result_types(func, &data, chain);
        let _ = write!(self.out, "{}", data.flags);
        self.operands(func, &data, chain);
        if let Some(mem) = chain.takes {
            self.out.push_str(" [mem ");
            self.value(mem);
            self.out.push(']');
        }
        self.out.push('\n');
    }

    /// The type suffix, where the operands do not already say what the result is.
    fn result_types(&mut self, func: &Func, data: &InstData, chain: Chain) {
        let results = without_mem(data.results().collect(), chain);
        match results.as_slice() {
            [] => {}
            _ if implied_result(data.opcode) => {}
            [result] => {
                let ty = func[*result].ty;
                let takes_the_same = func[data.args].first().is_some_and(|&arg| func[arg].ty == ty);
                if !takes_the_same {
                    let _ = write!(self.out, ".{ty}");
                }
            }
            // The handful that produce two. Both are written, because neither of them follows
            // from the operands in a way worth remembering a rule for.
            types => {
                self.out.push_str(".(");
                for (index, &result) in types.iter().enumerate() {
                    if index > 0 {
                        self.out.push_str(", ");
                    }
                    let _ = write!(self.out, "{}", func[result].ty);
                }
                self.out.push(')');
            }
        }
    }

    /// Everything to the right of the opcode.
    fn operands(&mut self, func: &Func, data: &InstData, chain: Chain) {
        let all = &func[data.args];
        let args = &all[..all.len() - usize::from(chain.takes.is_some())];
        match data.extra {
            Extra::None => self.value_list_spaced(args),
            Extra::Imm(imm) => {
                self.out.push(' ');
                let ty = data.first_result.map_or(Type::VOID, |result| func[result].ty);
                self.imm(func[imm], ty);
            }
            Extra::Symbol(symbol) => {
                let _ = write!(self.out, " @{}", self.names.resolve(symbol));
                if !args.is_empty() {
                    self.out.push('(');
                    self.value_list(args);
                    self.out.push(')');
                }
            }
            Extra::IntPred(pred) => {
                let _ = write!(self.out, " {}", pred.name());
                self.value_list_spaced(args);
            }
            Extra::FloatPred(pred) => {
                let _ = write!(self.out, " {}", pred.name());
                self.value_list_spaced(args);
            }
            Extra::Mem(mem) => {
                match (data.opcode, args) {
                    // A store reads left to right like the assignment it came from, which is
                    // worth one special case in the printer and one in the parser.
                    (Opcode::Store | Opcode::AtomicStore, [value, addr]) => {
                        self.out.push(' ');
                        self.value(*value);
                        self.out.push_str(" -> ");
                        self.value(*addr);
                    }
                    _ => self.value_list_spaced(args),
                }
                self.mem(func[mem]);
            }
            Extra::VaObject(info) => {
                let info = func[info];
                self.value_list_spaced(args);
                self.mem(func[info.mem]);
                let slots = &func[info.slots];
                if !slots.is_empty() {
                    self.out.push_str(", in(");
                    for (index, &slot) in slots.iter().enumerate() {
                        if index > 0 {
                            self.out.push_str(", ");
                        }
                        self.slot(slot);
                    }
                    self.out.push(')');
                }
            }
            Extra::Rmw(op, mem) => {
                let _ = write!(self.out, " {}", op.name());
                self.value_list_spaced(args);
                self.mem(func[mem]);
            }
            Extra::Order(order) => {
                let _ = write!(self.out, " {}", order.name());
            }
            Extra::Targets(targets) => {
                // A conditional branch names its condition first and then both arms. A jump
                // has no operands at all and is its target.
                if !args.is_empty() {
                    self.value_list_spaced(args);
                    self.out.push(',');
                }
                for (index, &call) in func[targets].iter().enumerate() {
                    self.out.push_str(if index > 0 { ", " } else { " " });
                    self.block_call(func, call);
                }
            }
            Extra::Call(call) => {
                let info = func[call];
                let rest = match info.callee {
                    Some(callee) => {
                        let _ = write!(self.out, " @{}", self.names.resolve(callee));
                        args
                    }
                    // An indirect call takes the address it calls as its first operand, and
                    // the rest are the arguments.
                    None => {
                        self.out.push(' ');
                        match args.split_first() {
                            Some((&addr, rest)) => {
                                self.value(addr);
                                rest
                            }
                            None => {
                                self.out.push_str("%?");
                                &[]
                            }
                        }
                    }
                };
                self.out.push('(');
                // An argument the signature names says how it travels there, and one past the
                // end of the list has nowhere else to say it than here.
                let named = func[info.signature].params.len();
                let varargs = &func[info.varargs];
                for (index, &arg) in rest.iter().enumerate() {
                    if index > 0 {
                        self.out.push_str(", ");
                    }
                    self.value(arg);
                    if let Some(&abi) = index.checked_sub(named).and_then(|at| varargs.get(at)) {
                        self.abi(abi);
                    }
                }
                self.out.push_str(") : ");
                self.signature(&func[info.signature]);
            }
            Extra::Switch(switch) => {
                let info = func[switch];
                let ty = args.first().map_or(Type::VOID, |&arg| func[arg].ty);
                self.value_list_spaced(args);
                if let Some((&default, cases)) = func[info.targets].split_first() {
                    self.out.push_str(", ");
                    self.block_call(func, default);
                    self.out.push_str(", [");
                    for (index, (&case, &value)) in cases.iter().zip(&func[info.cases]).enumerate()
                    {
                        if index > 0 {
                            self.out.push_str(", ");
                        }
                        self.imm(value, ty);
                        self.out.push_str(" => ");
                        self.block_call(func, case);
                    }
                    self.out.push(']');
                }
            }
            Extra::Asm(asm) => {
                let info = func[asm];
                self.out.push(' ');
                self.string(self.names.resolve(info.template).as_bytes());
                self.out.push_str(", ");
                self.string(self.names.resolve(info.constraints).as_bytes());
                self.out.push_str(", ");
                self.string(self.names.resolve(info.clobbers).as_bytes());
                self.out.push('(');
                self.value_list(args);
                self.out.push(')');
                if !info.targets.is_empty() {
                    self.out.push_str(", labels [");
                    for (index, &call) in func[info.targets].iter().enumerate() {
                        if index > 0 {
                            self.out.push_str(", ");
                        }
                        self.block_call(func, call);
                    }
                    self.out.push(']');
                }
            }
        }
    }

    /// The operands, separated by commas, with a leading space when there are any.
    fn value_list_spaced(&mut self, args: &[Value]) {
        if args.is_empty() {
            return;
        }
        self.out.push(' ');
        self.value_list(args);
    }

    /// The operands, separated by commas, with nothing in front.
    fn value_list(&mut self, args: &[Value]) {
        for (index, &arg) in args.iter().enumerate() {
            if index > 0 {
                self.out.push_str(", ");
            }
            self.value(arg);
        }
    }

    /// A branch target, with the values it passes.
    fn block_call(&mut self, func: &Func, call: BlockCall) {
        let _ = write!(self.out, "block{}", self.blocks[call.block.index()]);
        let args = &func[call.args];
        if !args.is_empty() {
            self.out.push('(');
            self.value_list(args);
            self.out.push(')');
        }
    }

    /// What an access carries beyond its address.
    fn mem(&mut self, info: MemInfo) {
        if info.size != 0 {
            let _ = write!(self.out, ", size {}", info.size);
        }
        let _ = write!(self.out, ", align {}", info.align);
        if info.order != MemOrder::NotAtomic {
            let _ = write!(self.out, ", {}", info.order.name());
        }
        if let Some(tbaa) = info.tbaa {
            let _ = write!(self.out, ", tbaa !{}", tbaa.index());
        }
        if info.restrict.clique != 0 {
            let _ =
                write!(self.out, ", restrict({}, {})", info.restrict.clique, info.restrict.base);
        }
    }

    /// One register's worth of an object, as what is read out of it and where its bytes are.
    fn slot(&mut self, slot: Slot) {
        match slot {
            Slot::Integer { offset, size } => {
                let _ = write!(self.out, "int {size} at {offset}");
            }
            Slot::Float { offset, format } => {
                let _ = write!(self.out, "float {} at {offset}", format.name());
            }
        }
    }

    /// One value, as the number it was given in print order.
    fn value(&mut self, value: Value) {
        match self.values.get(value.index()).copied() {
            Some(number) if number != u32::MAX => {
                let _ = write!(self.out, "%{number}");
            }
            // A use with no definition anywhere ahead of it. The verifier turns this down, and
            // printing something rather than panicking is what makes the printer usable for
            // finding out why.
            _ => self.out.push_str("%?"),
        }
    }

    /// One constant, read as the type it is a constant of.
    fn imm(&mut self, imm: Imm, ty: Type) {
        let scalar = if ty.is_vector() { ty.lane() } else { ty };
        if scalar.is_float() {
            // The bit pattern, because a decimal that reads back as the same value needs a
            // printer this compiler has not written yet, and because a NaN payload survives.
            let _ = write!(self.out, "{:#x}", imm.bits());
        } else if scalar.is_int() {
            let _ = write!(self.out, "{}", imm.signed(scalar));
        } else {
            let _ = write!(self.out, "{:#x}", imm.bits());
        }
    }

    /// One metadata node, on one line.
    fn meta_node(&mut self, meta: Meta) {
        let node = self.module[meta];
        let _ = write!(self.out, "!{} = tbaa ", meta.index());
        self.string(self.names.resolve(node.name).as_bytes());
        if let Some(parent) = node.parent {
            let _ = write!(self.out, ", parent !{}", parent.index());
        }
        let _ = write!(self.out, ", offset {}", node.offset);
        self.out.push('\n');
    }

    /// The linkage and, where it is not the ordinary one, the visibility.
    fn linkage(&mut self, linkage: Linkage, visibility: Visibility) {
        let _ = write!(self.out, ", linkage({})", linkage.name());
        if visibility != Visibility::Default {
            let _ = write!(self.out, ", visibility({})", visibility.name());
        }
    }

    /// The section, where one was asked for.
    fn section(&mut self, section: Option<Symbol>) {
        if let Some(section) = section {
            self.out.push_str(", section ");
            self.string(self.names.resolve(section).as_bytes());
        }
    }

    /// A byte string, quoted, with everything outside printable ASCII in hexadecimal.
    fn string(&mut self, bytes: &[u8]) {
        self.out.push('"');
        for &byte in bytes {
            match byte {
                b'"' => self.out.push_str("\\\""),
                b'\\' => self.out.push_str("\\\\"),
                0x20..=0x7e => self.out.push(byte as char),
                _ => {
                    let _ = write!(self.out, "\\{byte:02x}");
                }
            }
        }
        self.out.push('"');
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::*;
    use crate::Restrict;
    use crate::func::Builder;
    use crate::inst::{AsmInfo, CallInfo, MetaNode, SwitchInfo, VaInfo};
    use crate::module::{AliasKind, TlsModel};
    use crate::{AttrSet, Attrs, Flags, FloatPred, FpContract, IntPred, RmwOp};

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    #[test]
    fn the_example_in_the_spec() {
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("example.c"), &target());

        let char_node = module.add_meta(MetaNode {
            name: names.intern("omnipotent char"),
            parent: None,
            offset: 0,
        });
        let int_node = module.add_meta(MetaNode {
            name: names.intern("int"),
            parent: Some(char_node),
            offset: 0,
        });

        let i32_ = Type::int(32);
        let zero_bits = module.add_imm(Imm::int(0, i32_));
        let init = module.push_data(&[Datum::Scalar { ty: i32_, value: zero_bits }]);
        let mut counter = Global::new(names.intern("counter"), 4, 4);
        counter.linkage = Linkage::Internal;
        counter.init = Some(init);
        module.add_global(counter);

        let mut func = Func::new(
            names.intern("sum"),
            Signature::new().with_params(&[i32_]).with_returns(&[i32_]),
        );
        func.attrs = Attrs { set: AttrSet::NOUNWIND, fp_contract: FpContract::On };
        let entry = func.create_block();
        let n = func.append_param(entry, i32_);
        let header = func.create_block();
        let acc = func.append_param(header, i32_);
        let i = func.append_param(header, i32_);
        let exit = func.create_block();
        let result = func.append_param(exit, i32_);

        let mut b = Builder::new(&mut func, entry);
        let zero = b.iconst(i32_, 0);
        let cmp = b.icmp(IntPred::Sle, n, zero);
        b.br_if(cmp, exit, &[zero], header, &[zero, zero]);

        let mut b = Builder::new(&mut func, header);
        let one = b.iconst(i32_, 1);
        let next = b.binary(Opcode::Add, i, one, Flags::NSW);
        let total = b.binary(Opcode::Add, acc, next, Flags::NSW);
        let done = b.icmp(IntPred::Sge, next, n);
        b.br_if(done, exit, &[total], header, &[total, next]);

        let mut b = Builder::new(&mut func, exit);
        let address = b.value(
            InstData {
                extra: Extra::Symbol(names.intern("counter")),
                ..InstData::new(Opcode::GlobalAddr)
            },
            Type::PTR,
        );
        b.store(
            result,
            address,
            MemInfo {
                size: 0,
                align: 4,
                order: MemOrder::NotAtomic,
                tbaa: Some(int_node),
                restrict: Restrict::NONE,
            },
            Flags::NONE,
        );
        b.ret(&[result]);
        module.add_func(func);

        assert_eq!(print(&module, &names), crate::fixtures::EXAMPLE);
    }

    #[test]
    fn the_memory_safety_instructions() {
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("safety.c"), &target());
        let int_node =
            module.add_meta(MetaNode { name: names.intern("int"), parent: None, offset: 0 });

        let i64_ = Type::int(64);
        let mut func = Func::new(
            names.intern("safety"),
            Signature::new().with_params(&[Type::PTR, i64_]).with_returns(&[Type::PTR]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let off = func.append_param(entry, i64_);

        let mut b = Builder::new(&mut func, entry);
        let of = b.unary(Opcode::CapOf, p, Type::CAP);
        b.inst(InstData::new(Opcode::CapNull), &[Type::CAP]);
        b.unary(Opcode::CapRecover, p, Type::CAP);
        b.unary(Opcode::CapLoad, p, Type::CAP);
        let len = b.iconst(i64_, 8);
        let args = b.func().push_values(&[of, off, len]);
        let narrow = b.value(InstData { args, ..InstData::new(Opcode::CapNarrow) }, Type::CAP);
        let args = b.func().push_values(&[p, narrow]);
        b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);

        let args = b.func().push_values(&[p, off]);
        let derived = b.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        let four = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        };
        let mut check = |opcode, info: Option<MemInfo>, on: &[Value]| {
            let args = b.func().push_values(on);
            let extra = match info {
                Some(info) => Extra::Mem(b.func().add_mem(info)),
                None => Extra::None,
            };
            b.inst(InstData { args, extra, ..InstData::new(opcode) }, &[]);
        };
        check(Opcode::CheckBounds, Some(four), &[of, p]);
        check(Opcode::CheckLive, None, &[of, p]);
        check(Opcode::CheckType, Some(MemInfo { tbaa: Some(int_node), ..four }), &[of, p]);
        check(Opcode::CheckInit, Some(MemInfo { align: 1, ..four }), &[of, p]);
        check(Opcode::CheckDeriv, None, &[of, p, derived]);
        check(Opcode::CheckRace, None, &[of, p]);
        b.ret(&[p]);
        module.add_func(func);

        assert_eq!(print(&module, &names), crate::fixtures::SAFETY);
    }

    #[test]
    fn one_of_almost_everything() {
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("zoo.c"), &target());
        let int_node =
            module.add_meta(MetaNode { name: names.intern("int"), parent: None, offset: 0 });

        let i32_ = Type::int(32);
        let i64_ = Type::int(64);
        let f64_ = Type::float(crate::Float::F64);
        let mut func = Func::new(
            names.intern("zoo"),
            Signature::new().with_params(&[i32_, Type::PTR]).with_returns(&[i32_]),
        );
        let entry = func.create_block();
        let n = func.append_param(entry, i32_);
        let p = func.append_param(entry, Type::PTR);
        let middle = func.create_block();
        let other = func.create_block();
        let exit = func.create_block();
        let taken = func.append_param(exit, i32_);
        let arrival = func.create_block();

        let mut b = Builder::new(&mut func, entry);
        let minus_one = b.iconst(i64_, -1);
        let half = b.fconst(f64_, 0x3ff8_0000_0000_0000);
        let seven = b.func().add_imm(Imm::int(7, i32_));
        let vector = b.value(
            InstData { extra: Extra::Imm(seven), ..InstData::new(Opcode::Splat) },
            Type::vector(i32_, 4),
        );
        let stack = b.func().add_mem(MemInfo {
            size: 16,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        });
        let slot = b.value(
            InstData { extra: Extra::Mem(stack), ..InstData::new(Opcode::Alloca) },
            Type::PTR,
        );
        let args = b.func().push_values(&[slot, minus_one]);
        let addr = b.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        let plain = MemInfo {
            size: 0,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: Some(int_node),
            restrict: Restrict::NONE,
        };
        let loaded = b.load(i32_, addr, plain, Flags::NONE);
        b.store(loaded, addr, plain, Flags::VOLATILE);

        let atomic = b.func().add_mem(MemInfo {
            size: 0,
            align: 4,
            order: MemOrder::SeqCst,
            tbaa: None,
            restrict: Restrict::NONE,
        });
        let args = b.func().push_values(&[addr, n]);
        let old = b.value(
            InstData {
                args,
                extra: Extra::Rmw(RmwOp::Add, atomic),
                ..InstData::new(Opcode::AtomicRmw)
            },
            i32_,
        );
        let args = b.func().push_values(&[addr, old, n]);
        b.inst(
            InstData { args, extra: Extra::Mem(atomic), ..InstData::new(Opcode::Cmpxchg) },
            &[i32_, Type::I1],
        );
        b.inst(
            InstData { extra: Extra::Order(MemOrder::SeqCst), ..InstData::new(Opcode::Fence) },
            &[],
        );
        b.unary(Opcode::SExt, n, i64_);
        b.fcmp(FloatPred::Oeq, half, half, Flags::NONE);
        let args = b.func().push_values(&[n, n]);
        b.inst(InstData { args, ..InstData::new(Opcode::SAddOverflow) }, &[i32_, Type::I1]);
        let puts = b.func().add_signature(
            Signature::new().with_params(&[Type::PTR]).with_returns(&[i32_]).variadic(),
        );
        b.call_varargs(
            names.intern("puts"),
            puts,
            &[p, slot],
            &[Abi::ByVal { size: 16, align: 8 }],
        );
        let indirect =
            b.func().add_signature(Signature::new().with_params(&[i32_]).with_returns(&[i32_]));
        let varargs = b.func().push_abis(&[]);
        let info = b.func().add_call(CallInfo { callee: None, signature: indirect, varargs });
        let args = b.func().push_values(&[p, n]);
        b.value(
            InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::CallIndirect) },
            i32_,
        );
        let copy = b.func().add_mem(MemInfo {
            size: 16,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        });
        let args = b.func().push_values(&[slot, p]);
        b.inst(InstData { args, extra: Extra::Mem(copy), ..InstData::new(Opcode::Memcpy) }, &[]);
        let asm = b.func().add_asm(AsmInfo {
            template: names.intern("pause"),
            constraints: names.intern(""),
            clobbers: names.intern("memory"),
            targets: crate::inst::BlockCallList::EMPTY,
        });
        b.inst(
            InstData {
                flags: Flags::VOLATILE,
                extra: Extra::Asm(asm),
                ..InstData::new(Opcode::InlineAsm)
            },
            &[],
        );
        let object = b.func().add_mem(MemInfo {
            size: 16,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        });
        let slots = b.func().push_slots(&[
            Slot::Integer { offset: 0, size: 8 },
            Slot::Float { offset: 8, format: rucc_base::float::Format::Double },
        ]);
        let read = b.func().add_va_object(VaInfo { mem: object, slots });
        let args = b.func().push_values(&[p]);
        b.value(
            InstData { args, extra: Extra::VaObject(read), ..InstData::new(Opcode::VaObject) },
            Type::PTR,
        );
        let args = b.func().push_values(&[vector]);
        b.value(
            InstData {
                args,
                extra: Extra::Symbol(names.intern("x86.sse2.pmovmskb")),
                ..InstData::new(Opcode::TargetIntrinsic)
            },
            i32_,
        );
        b.jump(middle, &[]);

        let mut b = Builder::new(&mut func, middle);
        let cases = b.func().push_imms(&[Imm::int(0, i32_), Imm::int(-1, i32_)]);
        let default = BlockCall { block: other, args: crate::inst::ValueList::EMPTY };
        let first = BlockCall { block: exit, args: b.func().push_values(&[n]) };
        let second = BlockCall { block: other, args: crate::inst::ValueList::EMPTY };
        let targets = b.func().push_block_calls(&[default, first, second]);
        let switch = b.func().add_switch(SwitchInfo { targets, cases });
        let args = b.func().push_values(&[n]);
        b.inst(
            InstData { args, extra: Extra::Switch(switch), ..InstData::new(Opcode::Switch) },
            &[],
        );

        let mut b = Builder::new(&mut func, other);
        let address = b.block_addr(arrival);
        b.indirect_br(address, &[arrival]);

        let mut b = Builder::new(&mut func, exit);
        b.ret(&[taken]);

        let mut b = Builder::new(&mut func, arrival);
        let call = BlockCall { block: exit, args: b.func().push_values(&[n]) };
        let targets = b.func().push_block_calls(&[call]);
        let goto = b.func().add_asm(AsmInfo {
            template: names.intern("jmp %l0"),
            constraints: names.intern(""),
            clobbers: names.intern(""),
            targets,
        });
        b.inst(InstData { extra: Extra::Asm(goto), ..InstData::new(Opcode::InlineAsm) }, &[]);

        module.add_func(func);

        assert_eq!(print(&module, &names), crate::fixtures::ZOO);
    }

    #[test]
    fn the_shapes_a_symbol_comes_in() {
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("data.c"), &target());

        let i32_ = Type::int(32);
        let text = module.push_bytes(b"hi\x00\xff\"\\");
        let entry_name = names.intern("hi.str");
        let forward = module.add_reloc(Reloc { symbol: entry_name, addend: 8, size: 8 });
        let backward = module.add_reloc(Reloc { symbol: entry_name, addend: -8, size: 8 });
        let seven = module.add_imm(Imm::int(7, i32_));
        let image = module.push_data(&[
            Datum::Bytes(text),
            Datum::Zero(2),
            Datum::Scalar { ty: i32_, value: seven },
            Datum::Addr(forward),
            Datum::Addr(backward),
        ]);
        let mut table = Global::new(names.intern("table"), 28, 8);
        table.init = Some(image);
        table.constant = true;
        table.section = Some(names.intern(".rodata.rel"));
        module.add_global(table);

        let mut errno = Global::new(names.intern("errno"), 4, 4);
        errno.tls = Some(TlsModel::InitialExec);
        errno.visibility = Visibility::Hidden;
        module.add_global(errno);

        // A zero sized object with an initialiser, which `char x[0] = { };` at file scope is.
        // The image is there and has nothing in it, which is not the same as the global that has
        // no image at all, and the two have to print differently for the reader to tell them
        // apart.
        let mut nothing = Global::new(names.intern("nothing"), 0, 1);
        nothing.init = Some(module.push_data(&[]));
        nothing.linkage = Linkage::Internal;
        module.add_global(nothing);

        let mut alias = Alias::new(names.intern("total"), names.intern("table"));
        alias.linkage = Linkage::Weak;
        module.add_alias(alias);
        let mut memcpy = Alias::new(names.intern("memcpy"), names.intern("memcpy.resolve"));
        memcpy.kind = AliasKind::IFunc;
        memcpy.visibility = Visibility::Protected;
        module.add_alias(memcpy);

        let mut puts = Func::new(
            names.intern("puts"),
            Signature::new().with_params(&[Type::PTR]).with_returns(&[i32_]),
        );
        puts.linkage = Linkage::External;
        puts.attrs.set = AttrSet::NOUNWIND | AttrSet::WILLRETURN;
        module.add_func(puts);

        let mut helper =
            Func::new(names.intern("helper"), Signature::new().with_returns(&[i32_, i32_]));
        helper.linkage = Linkage::Internal;
        helper.section = Some(names.intern(".text.hot"));
        helper.attrs.set = AttrSet::READNONE | AttrSet::ALWAYS_INLINE;
        let block = helper.create_block();
        let mut b = Builder::new(&mut helper, block);
        let one = b.iconst(i32_, 1);
        b.ret(&[one, one]);
        module.add_func(helper);

        assert_eq!(print(&module, &names), crate::fixtures::SYMBOLS);
    }

    #[test]
    fn a_signature_writes_what_the_abi_asks_of_each_parameter() {
        let mut names = Interner::new();
        let module = Module::new(names.intern("abi.c"), &target());
        let mut func = Func::new(
            names.intern("f"),
            Signature::new()
                .and_param(Param::with_abi(Type::PTR, Abi::Sret { size: 24, align: 8 }))
                .and_param(Param::with_abi(Type::PTR, Abi::ByVal { size: 16, align: 8 }))
                .and_param(Param::with_abi(Type::int(8), Abi::Zext))
                .and_param(Param::new(Type::int(32))),
        );
        let entry = func.create_block();
        for param in [Type::PTR, Type::PTR, Type::int(8), Type::int(32)] {
            func.append_param(entry, param);
        }
        let mut b = Builder::new(&mut func, entry);
        b.ret(&[]);

        assert_eq!(
            print_func(&module, &func, &names),
            "\
func @f(ptr sret(24, align 8), ptr byval(16, align 8), i8 zext, i32), linkage(external) {
block0(%0: ptr, %1: ptr, %2: i8, %3: i32):
    return
}
"
        );
    }

    #[test]
    fn a_call_writes_what_the_abi_asks_of_an_argument_its_signature_does_not_name() {
        let mut names = Interner::new();
        let module = Module::new(names.intern("varargs.c"), &target());
        let i32_ = Type::int(32);
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[Type::PTR]));
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let mut b = Builder::new(&mut func, entry);
        let sig = b.func().add_signature(
            Signature::new().with_params(&[Type::PTR]).with_returns(&[i32_]).variadic(),
        );
        let one = b.iconst(i32_, 1);
        b.call_varargs(
            names.intern("printf"),
            sig,
            &[p, one, p],
            &[Abi::Plain, Abi::ByVal { size: 24, align: 8 }],
        );
        b.ret(&[]);

        assert_eq!(
            print_func(&module, &func, &names),
            "\
func @f(ptr), linkage(external) {
block0(%0: ptr):
    %1 = iconst.i32 1
    %2 = call @printf(%0, %1, %0 byval(24, align 8)) : (ptr, ...) -> i32
    return
}
"
        );
    }

    #[test]
    fn numbering_follows_the_text_and_not_the_tables() {
        // The blocks are laid out entry, middle, exit, and their contents are built in the
        // opposite order, so every index in the tables runs against the order they print in.
        // The numbers in the text have to come out in reading order anyway, because that is
        // what makes printing a module, parsing it and printing it again give the same bytes.
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("order.c"), &target());
        let i32_ = Type::int(32);
        let mut func = Func::new(names.intern("f"), Signature::new().with_returns(&[i32_]));
        let entry = func.create_block();
        let middle = func.create_block();
        let exit = func.create_block();
        let arrived = func.append_param(exit, i32_);

        let mut b = Builder::new(&mut func, exit);
        b.ret(&[arrived]);
        let mut b = Builder::new(&mut func, middle);
        let two = b.iconst(i32_, 2);
        b.jump(exit, &[two]);
        let mut b = Builder::new(&mut func, entry);
        b.jump(middle, &[]);
        module.add_func(func);

        assert_eq!(
            print_func(&module, &module[module.funcs().next().unwrap()], &names),
            "\
func @f() -> i32, linkage(external) {
block0:
    jump block1

block1:
    %0 = iconst.i32 2
    jump block2(%0)

block2(%1: i32):
    return %1
}
"
        );
    }
}
