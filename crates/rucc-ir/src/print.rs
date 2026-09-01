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

use crate::func::Func;
use crate::inst::{Block, BlockCall, Imm, Inst, InstData, MemInfo, Meta, Signature, Value};
use crate::module::{Alias, Datum, Global, Module, Reloc};
use crate::{Extra, FORMAT_VERSION, Linkage, MemOrder, Opcode, Type, Visibility};

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
            | Opcode::Alloca
            | Opcode::Call
            | Opcode::CallIndirect
            | Opcode::TailCall
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
                    self.out.push_str(" = { ");
                    for (index, &datum) in self.module[init].iter().enumerate() {
                        if index > 0 {
                            self.out.push_str(", ");
                        }
                        self.datum(datum);
                    }
                    self.out.push_str(" }");
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

    /// The parameter and result types of a function or a call.
    fn signature(&mut self, signature: &Signature) {
        self.out.push('(');
        for (index, &ty) in signature.params.iter().enumerate() {
            if index > 0 {
                self.out.push_str(", ");
            }
            let _ = write!(self.out, "{ty}");
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
            [ty] => {
                let _ = write!(self.out, " -> {ty}");
            }
            types => {
                self.out.push_str(" -> (");
                for (index, &ty) in types.iter().enumerate() {
                    if index > 0 {
                        self.out.push_str(", ");
                    }
                    let _ = write!(self.out, "{ty}");
                }
                self.out.push(')');
            }
        }
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
        self.result_types(func, &data);
        let _ = write!(self.out, "{}", data.flags);
        self.operands(func, &data);
        self.out.push('\n');
    }

    /// The type suffix, where the operands do not already say what the result is.
    fn result_types(&mut self, func: &Func, data: &InstData) {
        let results: Vec<Value> = data.results().collect();
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
    fn operands(&mut self, func: &Func, data: &InstData) {
        let args = &func[data.args];
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
                self.value_list(rest);
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
    use crate::func::Builder;
    use crate::inst::{AsmInfo, CallInfo, MetaNode, SwitchInfo};
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
            MemInfo { size: 0, align: 4, order: MemOrder::NotAtomic, tbaa: Some(int_node) },
            Flags::NONE,
        );
        b.ret(&[result]);
        module.add_func(func);

        assert_eq!(
            print(&module, &names),
            "\
; ModuleID = 'example.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"

global @counter : i32 = 0, align 4, linkage(internal)

func @sum(i32) -> i32, linkage(external), attrs(nounwind, fp_contract=on) {
block0(%0: i32):
    %1 = iconst.i32 0
    %2 = icmp sle %0, %1
    br_if %2, block2(%1), block1(%1, %1)

block1(%3: i32, %4: i32):
    %5 = iconst.i32 1
    %6 = add.nsw %4, %5
    %7 = add.nsw %3, %6
    %8 = icmp sge %6, %0
    br_if %8, block2(%7), block1(%7, %6)

block2(%9: i32):
    %10 = global_addr @counter
    store %9 -> %10, align 4, tbaa !1
    return %9
}

!0 = tbaa \"omnipotent char\", offset 0
!1 = tbaa \"int\", parent !0, offset 0
"
        );
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
        let mut func =
            Func::new(names.intern("zoo"), Signature::new().with_params(&[i32_, Type::PTR]));
        let entry = func.create_block();
        let n = func.append_param(entry, i32_);
        let p = func.append_param(entry, Type::PTR);
        let middle = func.create_block();
        let other = func.create_block();
        let exit = func.create_block();
        let taken = func.append_param(exit, i32_);

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
        });
        let slot = b.value(
            InstData { extra: Extra::Mem(stack), ..InstData::new(Opcode::Alloca) },
            Type::PTR,
        );
        let args = b.func().push_values(&[slot, minus_one]);
        let addr = b.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        let plain = MemInfo { size: 0, align: 4, order: MemOrder::NotAtomic, tbaa: Some(int_node) };
        let loaded = b.load(i32_, addr, plain, Flags::NONE);
        b.store(loaded, addr, plain, Flags::VOLATILE);

        let atomic =
            b.func().add_mem(MemInfo { size: 0, align: 4, order: MemOrder::SeqCst, tbaa: None });
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
        b.call(names.intern("puts"), puts, &[p]);
        let indirect =
            b.func().add_signature(Signature::new().with_params(&[i32_]).with_returns(&[i32_]));
        let info = b.func().add_call(CallInfo { callee: None, signature: indirect });
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
        let call = BlockCall { block: exit, args: b.func().push_values(&[n]) };
        let targets = b.func().push_block_calls(&[call]);
        let goto = b.func().add_asm(AsmInfo {
            template: names.intern("jmp %l0"),
            constraints: names.intern(""),
            clobbers: names.intern(""),
            targets,
        });
        b.inst(InstData { extra: Extra::Asm(goto), ..InstData::new(Opcode::InlineAsm) }, &[]);

        let mut b = Builder::new(&mut func, exit);
        b.ret(&[taken]);

        module.add_func(func);

        assert_eq!(
            print(&module, &names),
            "\
; ModuleID = 'zoo.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"

func @zoo(i32, ptr), linkage(external) {
block0(%0: i32, %1: ptr):
    %2 = iconst.i64 -1
    %3 = fconst.f64 0x3ff8000000000000
    %4 = splat.i32x4 7
    %5 = alloca, size 16, align 8
    %6 = ptr_add %5, %2
    %7 = load.i32 %6, align 4, tbaa !0
    store.volatile %7 -> %6, align 4, tbaa !0
    %8 = atomic_rmw.i32 add %6, %0, align 4, seq_cst
    %9, %10 = cmpxchg.(i32, i1) %6, %8, %0, align 4, seq_cst
    fence seq_cst
    %11 = sext.i64 %0
    %12 = fcmp oeq %3, %3
    %13, %14 = sadd_overflow.(i32, i1) %0, %0
    %15 = call @puts(%1) : (ptr, ...) -> i32
    %16 = call_indirect %1(%0) : (i32) -> i32
    memcpy %5, %1, size 16, align 8
    inline_asm.volatile \"pause\", \"\", \"memory\"()
    %17 = target_intrinsic.i32 @x86.sse2.pmovmskb(%4)
    jump block1

block1:
    switch %0, block2, [0 => block3(%0), -1 => block2]

block2:
    inline_asm \"jmp %l0\", \"\", \"\"(), labels [block3(%0)]

block3(%18: i32):
    return %18
}

!0 = tbaa \"int\", offset 0
"
        );
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

        assert_eq!(
            print(&module, &names),
            "\
; ModuleID = 'data.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"

global @table : bytes 28 = { bytes \"hi\\00\\ff\\\"\\\\\", zero 2, i32 7, addr.8 @hi.str + 8, addr.8 @hi.str - 8 }, align 8, linkage(external), constant, section \".rodata.rel\"
global @errno : bytes 4, align 4, linkage(external), visibility(hidden), tls(initial_exec)

alias @total = @table, linkage(weak)
ifunc @memcpy = @memcpy.resolve, linkage(external), visibility(protected)

func @puts(ptr) -> i32, linkage(external), attrs(nounwind, willreturn);

func @helper() -> (i32, i32), linkage(internal), attrs(always_inline, readnone), section \".text.hot\" {
block0:
    %0 = iconst.i32 1
    return %0, %0
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
