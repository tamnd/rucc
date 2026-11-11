//! The integer that is wider than two registers, as the words it is kept in memory as.
//!
//! A `_BitInt` may be up to 65535 bits wide on x86-64 and on AArch64, which is what gcc allows on
//! both, and nothing below this pass has a value that wide. [`crate::wide`] splits one of a hundred
//! and twenty eight bits into two registers. That does not scale: a value of a thousand words in a
//! thousand registers is a value the allocator spills anyway, and a shift by an amount only known
//! when the program runs has to pick a word by that amount, which a register cannot be.
//!
//! So this pass does what gcc's `_BitInt` lowering does at the same widths. Every value wider than
//! a hundred and twenty eight bits gets a slot in the frame, and the value is the words in that
//! slot. Each instruction over such a value becomes loads of the words it reads, the arithmetic on
//! them, and stores of the words it writes. After it there is no value wider than a hundred and
//! twenty eight bits anywhere in the function.
//!
//! # The form of a value in its slot
//!
//! `ceil(N / 64)` words of sixty four bits, the least significant first, which is the order the
//! psABI and the AAPCS64 both give a `_BitInt` in memory. The top word is always sign extended from
//! bit `N - 1`. The IR does not say whether a value is signed, and a value kept this way compares
//! correctly both ways: the extension maps the unsigned values of `N` bits onto the words in the
//! same order, and the signed ones onto the signed words. So every instruction that writes a top
//! word writes it extended, and nothing has to ask which kind it has. The ones that read a value as
//! unsigned, a logical shift, a zero extension, an unsigned division and the unsigned conversions,
//! clear the bits above `N` on the copy they read.
//!
//! # What is a call
//!
//! A multiplication, a division, a remainder, and a conversion to or from a float call the libgcc
//! routines gcc calls for them: `__mulbitint3`, `__divmodbitint4`, `__floatbitint*` and
//! `__fix*bitint`. Each takes the words by address and a precision, which is `-N` for a signed
//! value and `N` for an unsigned one. The words a product, a quotient or a remainder is written
//! into have the precision `N` whatever the signedness, as gcc passes it. Everything else is written out word by word here. An addition
//! is a carry chain, a comparison is a walk from the bottom word up, and a shift reads its words
//! from a buffer at an offset the shift amount picks.
//!
//! # What crosses an edge
//!
//! A block parameter gets a slot of its own in the entry block, and an edge into the block copies
//! the argument's words into it. When an argument is another parameter of the same block, the copy
//! goes through a slot of its own first, so that a swap of two parameters is a swap. An edge from a
//! branch with two ways out gets a block of its own for the copy, so that the way not taken does not
//! write a slot that is still being read.
//!
//! # What is left alone
//!
//! A function with anything else on a value this wide: an overflow check, an atomic access, a bit
//! count, an `asm` operand, or a conversion against the eighty bit format. The whole function is
//! left as it was, and the selector refuses it by naming the instruction it had no rule for.
//! Parameters and returns never reach here at these widths, since both conventions pass such a
//! value through memory and `rucc-lower` has already written that down.

use std::collections::HashMap;

use rucc_base::Interner;
use rucc_ir::{
    Block, BlockCall, Builder, CallInfo, Extra, Flags, Float, Func, Imm, Inst, InstData, IntPred,
    MemInfo, MemOrder, Opcode, Param, Restrict, Signature, Type, Value,
};

/// One word of a value, which is a register on every machine this pass runs for.
const WORD: Type = Type::int(64);

/// The widest integer the rest of the back end takes. [`crate::wide`] splits one this wide.
const SPLIT: u32 = 128;

/// Whether a value of this type is one this pass keeps in words.
fn wide(ty: Type) -> bool {
    ty.is_int() && ty.is_scalar() && ty.bits() > SPLIT
}

/// How wide an integer value is.
fn width(func: &Func, value: Value) -> u32 {
    func[value].ty.bits()
}

/// How many words a value of this width is kept in.
fn words(bits: u32) -> u32 {
    bits.div_ceil(64)
}

/// Rewrites every value wider than a hundred and twenty eight bits into the words in its slot.
///
/// `word` is the size of a register in bytes. Only a machine with sixty four bit registers lets a
/// `_BitInt` this wide, so any other answer leaves the function alone.
///
/// Answers whether it rewrote the function, which is `false` for a function with no value this
/// wide in it and for one with something on such a value it does not know. See the module's last
/// section for the second.
pub fn limbs(func: &mut Func, names: &mut Interner, word: u32) -> bool {
    if word != 8 || !func.values().any(|value| wide(func[value].ty)) {
        return false;
    }
    let insts: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    if !insts.iter().all(|&inst| understood(func, inst)) {
        return false;
    }
    let Some(entry) = func.entry() else { return false };
    if func[entry].first.is_none() {
        return false;
    }

    // Every slot is a fixed alloca, and those belong in the entry block. The entry block is above
    // every edge that writes a parameter's slot and every instruction that reads a result's slot.
    let mut slots = HashMap::new();
    let blocks: Vec<Block> = func.blocks().collect();
    let mut params = Vec::new();
    for &block in &blocks {
        for param in func[block].params.clone() {
            let ty = func[param].ty;
            if wide(ty) {
                slots.insert(param, slot(func, words(ty.bits())));
                params.push(param);
            }
        }
    }
    for &inst in &insts {
        let results: Vec<Value> = func[inst].results().collect();
        for result in results {
            let ty = func[result].ty;
            if wide(ty) {
                slots.insert(result, slot(func, words(ty.bits())));
            }
        }
    }

    let mut pass = Pass { names, slots, renamed: HashMap::new() };
    for &inst in &insts {
        if !func.is_terminator(inst) {
            pass.rewrite(func, inst);
        }
    }
    for &inst in &insts {
        if func.is_terminator(inst) {
            pass.edges(func, inst);
        }
    }

    // A parameter is now the address of its own slot, which every edge passes. The results of the
    // instructions taken out are values nothing reads, and they are given a width the rest of the
    // back end has so that nothing that walks every value meets one this wide.
    for param in params {
        func.retype(param, Type::PTR);
    }
    let values: Vec<Value> = func.values().collect();
    for value in values {
        if wide(func[value].ty) {
            func.retype(value, WORD);
        }
    }
    let renamed = pass.renamed;
    if !renamed.is_empty() {
        func.rewrite_all(|value| renamed.get(&value).copied().unwrap_or(value));
    }
    true
}

/// Whether this pass knows what to do with an instruction, which is every instruction that does
/// not touch a value this wide and the ones listed here that do.
fn understood(func: &Func, inst: Inst) -> bool {
    let data = &func[inst];
    let args = &func[data.args];
    let edges = func.successors(inst).any(|call| func[call.args].iter().any(|&v| wide(func[v].ty)));
    if edges && !matches!(data.extra, Extra::Targets(_)) {
        return false;
    }
    let touches = args.iter().any(|&value| wide(func[value].ty))
        || data.results().any(|value| wide(func[value].ty));
    if !touches {
        return true;
    }
    match data.opcode {
        Opcode::IConst
        | Opcode::Add
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
        | Opcode::AShr
        | Opcode::ICmp
        | Opcode::Select
        | Opcode::Trunc
        | Opcode::SExt
        | Opcode::ZExt
        | Opcode::Load
        | Opcode::Store
        | Opcode::PtrToInt
        | Opcode::IntToPtr
        | Opcode::Expect => data.results <= 1,
        Opcode::SIToFP | Opcode::UIToFP => {
            data.results().next().is_some_and(|value| mode(func[value].ty).is_some())
        }
        Opcode::FPToSI | Opcode::FPToUI => {
            args.first().is_some_and(|&v| mode(func[v].ty).is_some())
        }
        _ => false,
    }
}

/// The letters libgcc names a float format by in its `_BitInt` conversions, for the formats this
/// pass converts. The eighty bit format travels through memory in a call and is left alone, and
/// so are the decimal and brain formats.
fn mode(ty: Type) -> Option<&'static str> {
    if !ty.is_scalar() {
        return None;
    }
    match ty.format()? {
        Float::F16 => Some("hf"),
        Float::F32 => Some("sf"),
        Float::F64 => Some("df"),
        Float::F128 => Some("tf"),
        _ => None,
    }
}

/// What the pass carries from one instruction to the next.
struct Pass<'a> {
    /// Where the names of the routines it calls are kept.
    names: &'a mut Interner,
    /// The slot each value this wide is kept in.
    slots: HashMap<Value, Value>,
    /// A value narrow enough to keep that an instruction over a wide one produced, and the value
    /// that stands for it now.
    renamed: HashMap<Value, Value>,
}

impl Pass<'_> {
    /// The slot a value this wide is kept in.
    fn held(&self, value: Value) -> Value {
        self.slots[&value]
    }

    /// Rewrites one instruction that is not a terminator, if it touches a value this wide.
    fn rewrite(&mut self, func: &mut Func, inst: Inst) {
        let data = func[inst];
        let args = func[data.args].to_vec();
        let result = data.first_result;
        let into = result.and_then(|value| self.slots.get(&value).copied());
        let touches = into.is_some() || args.iter().any(|value| self.slots.contains_key(value));
        if !touches {
            return;
        }
        match data.opcode {
            Opcode::IConst => {
                let (Some(into), Some(result), Extra::Imm(imm)) = (into, result, data.extra) else {
                    return;
                };
                let value = func[imm].signed(func[result].ty);
                let count = words(width(func, result));
                for at in 0..count {
                    let word = match at {
                        0 => value as i64,
                        1 => (value >> 64) as i64,
                        _ => (value >> 127) as i64,
                    };
                    let word = constant(func, inst, WORD, i128::from(word));
                    write(func, inst, word, into, at);
                }
            }
            Opcode::Add | Opcode::Sub => {
                let (Some(into), Some(result)) = (into, result) else { return };
                let n = width(func, result);
                let (a, b) = (self.held(args[0]), self.held(args[1]));
                let add = data.opcode == Opcode::Add;
                let mut carry = None;
                let count = words(n);
                for at in 0..count {
                    let x = read(func, inst, a, at);
                    let y = read(func, inst, b, at);
                    let mut sum = op(func, inst, data.opcode, &[x, y], WORD);
                    let pred = IntPred::Ult;
                    // The carry out of an addition is a sum below either operand. The borrow out
                    // of a subtraction is the first operand below the second.
                    let mut out = if add {
                        less(func, inst, pred, sum, x)
                    } else {
                        less(func, inst, pred, x, y)
                    };
                    if let Some(carry) = carry {
                        let before = sum;
                        sum = op(func, inst, data.opcode, &[sum, carry], WORD);
                        let again = if add {
                            less(func, inst, pred, sum, before)
                        } else {
                            less(func, inst, pred, before, carry)
                        };
                        out = op(func, inst, Opcode::Or, &[out, again], WORD);
                    }
                    if at + 1 == count {
                        sum = extended(func, inst, sum, n);
                    }
                    write(func, inst, sum, into, at);
                    carry = Some(out);
                }
            }
            Opcode::And | Opcode::Or | Opcode::Xor => {
                let (Some(into), Some(result)) = (into, result) else { return };
                let (a, b) = (self.held(args[0]), self.held(args[1]));
                for at in 0..words(width(func, result)) {
                    let x = read(func, inst, a, at);
                    let y = read(func, inst, b, at);
                    let word = op(func, inst, data.opcode, &[x, y], WORD);
                    write(func, inst, word, into, at);
                }
            }
            Opcode::Shl | Opcode::LShr | Opcode::AShr => {
                let Some(result) = result else { return };
                let amount = self.amount(func, inst, args[1]);
                match into {
                    Some(into) => {
                        shift(
                            func,
                            inst,
                            data.opcode,
                            self.held(args[0]),
                            amount,
                            into,
                            width(func, result),
                        );
                    }
                    // A narrow value shifted by a wide amount, which is the amount's bottom word.
                    None => {
                        let ty = func[result].ty;
                        let amount = narrowed(func, inst, amount, ty);
                        let value = op(func, inst, data.opcode, &[args[0], amount], ty);
                        self.renamed.insert(result, value);
                    }
                }
            }
            Opcode::ICmp => {
                let (Some(result), Extra::IntPred(pred)) = (result, data.extra) else { return };
                let n = width(func, args[0]);
                let (a, b) = (self.held(args[0]), self.held(args[1]));
                let answer = compare(func, inst, pred, a, b, n);
                self.renamed.insert(result, answer);
            }
            Opcode::Select => {
                let (Some(into), Some(result)) = (into, result) else { return };
                let (a, b) = (self.held(args[1]), self.held(args[2]));
                for at in 0..words(width(func, result)) {
                    let x = read(func, inst, a, at);
                    let y = read(func, inst, b, at);
                    let word = op(func, inst, Opcode::Select, &[args[0], x, y], WORD);
                    write(func, inst, word, into, at);
                }
            }
            Opcode::Expect => {
                let (Some(into), Some(result)) = (into, result) else { return };
                copy(func, inst, self.held(args[0]), into, words(width(func, result)));
            }
            Opcode::Trunc => {
                let Some(result) = result else { return };
                let from = self.held(args[0]);
                let ty = func[result].ty;
                match into {
                    Some(into) => {
                        let n = ty.bits();
                        let count = words(n);
                        for at in 0..count {
                            let mut word = read(func, inst, from, at);
                            if at + 1 == count {
                                word = extended(func, inst, word, n);
                            }
                            write(func, inst, word, into, at);
                        }
                    }
                    None => {
                        let value = bottom(func, inst, from, ty);
                        self.renamed.insert(result, value);
                    }
                }
            }
            Opcode::SExt | Opcode::ZExt => {
                let (Some(into), Some(result)) = (into, result) else { return };
                let signed = data.opcode == Opcode::SExt;
                self.extend(func, inst, args[0], into, width(func, result), signed);
            }
            Opcode::PtrToInt => {
                let (Some(into), Some(result)) = (into, result) else { return };
                let low = op(func, inst, Opcode::PtrToInt, &args[..1], WORD);
                let zero = constant(func, inst, WORD, 0);
                write(func, inst, low, into, 0);
                for at in 1..words(width(func, result)) {
                    write(func, inst, zero, into, at);
                }
            }
            Opcode::IntToPtr => {
                let Some(result) = result else { return };
                let low = read(func, inst, self.held(args[0]), 0);
                let value = op(func, inst, Opcode::IntToPtr, &[low], Type::PTR);
                self.renamed.insert(result, value);
            }
            Opcode::Load => {
                let (Some(into), Some(result), Extra::Mem(mem)) = (into, result, data.extra) else {
                    return;
                };
                let align = func[mem].align.clamp(1, 8);
                let n = width(func, result);
                let count = words(n);
                for at in 0..count {
                    let address = offset(func, inst, args[0], u64::from(at) * 8);
                    let mut word = load(func, inst, address, align);
                    if at + 1 == count {
                        word = extended(func, inst, word, n);
                    }
                    write(func, inst, word, into, at);
                }
            }
            Opcode::Store => {
                let Extra::Mem(mem) = data.extra else { return };
                let align = func[mem].align.clamp(1, 8);
                let from = self.held(args[0]);
                for at in 0..words(width(func, args[0])) {
                    let word = read(func, inst, from, at);
                    let address = offset(func, inst, args[1], u64::from(at) * 8);
                    store(func, inst, word, address, align);
                }
            }
            Opcode::Mul => {
                let (Some(into), Some(result)) = (into, result) else { return };
                let n = width(func, result);
                let precision = signed_precision(n);
                let precision = constant(func, inst, Type::int(32), i128::from(precision));
                // The precision of what libgcc writes is `N` whatever the signedness, which is
                // what gcc passes. A negative one is read as a count of limbs past four billion.
                let out = unsigned_precision(n);
                let out = constant(func, inst, Type::int(32), i128::from(out));
                let (a, b) = (self.held(args[0]), self.held(args[1]));
                let call = [into, out, a, precision, b, precision];
                self.call(func, inst, "__mulbitint3", &call, None);
                settle(func, inst, into, n);
            }
            Opcode::SDiv | Opcode::UDiv | Opcode::SRem | Opcode::URem => {
                let (Some(into), Some(result)) = (into, result) else { return };
                let n = width(func, result);
                let signed = matches!(data.opcode, Opcode::SDiv | Opcode::SRem);
                let precision = if signed { signed_precision(n) } else { unsigned_precision(n) };
                let precision = constant(func, inst, Type::int(32), i128::from(precision));
                let (mut a, mut b) = (self.held(args[0]), self.held(args[1]));
                if !signed {
                    a = cleared(func, inst, a, n);
                    b = cleared(func, inst, b, n);
                }
                let zero = constant(func, inst, WORD, 0);
                let none = op(func, inst, Opcode::IntToPtr, &[zero], Type::PTR);
                let unused = constant(func, inst, Type::int(32), 0);
                // What libgcc writes has the precision `N` either way, as for a product.
                let out = unsigned_precision(n);
                let out = constant(func, inst, Type::int(32), i128::from(out));
                let quotient = matches!(data.opcode, Opcode::SDiv | Opcode::UDiv);
                let call = if quotient {
                    [into, out, none, unused, a, precision, b, precision]
                } else {
                    [none, unused, into, out, a, precision, b, precision]
                };
                self.call(func, inst, "__divmodbitint4", &call, None);
                settle(func, inst, into, n);
            }
            Opcode::SIToFP | Opcode::UIToFP => {
                let Some(result) = result else { return };
                let ty = func[result].ty;
                let Some(mode) = mode(ty) else { return };
                let n = width(func, args[0]);
                let mut from = self.held(args[0]);
                let precision = if data.opcode == Opcode::SIToFP {
                    signed_precision(n)
                } else {
                    from = cleared(func, inst, from, n);
                    unsigned_precision(n)
                };
                let precision = constant(func, inst, Type::int(32), i128::from(precision));
                let routine = format!("__floatbitint{mode}");
                let value = self.call(func, inst, &routine, &[from, precision], Some(ty));
                if let Some(value) = value {
                    self.renamed.insert(result, value);
                }
            }
            Opcode::FPToSI | Opcode::FPToUI => {
                let (Some(into), Some(result)) = (into, result) else { return };
                let Some(mode) = mode(func[args[0]].ty) else { return };
                let n = width(func, result);
                let precision = if data.opcode == Opcode::FPToSI {
                    signed_precision(n)
                } else {
                    unsigned_precision(n)
                };
                let precision = constant(func, inst, Type::int(32), i128::from(precision));
                let routine = format!("__fix{mode}bitint");
                self.call(func, inst, &routine, &[into, precision, args[0]], None);
                settle(func, inst, into, n);
            }
            _ => return,
        }
        func.remove_inst(inst);
    }

    /// The amount a shift reads, as a word: the bottom word of a wide one, and a narrow one widened
    /// or narrowed to a word.
    fn amount(&self, func: &mut Func, inst: Inst, amount: Value) -> Value {
        if let Some(&slot) = self.slots.get(&amount) {
            return read(func, inst, slot, 0);
        }
        let bits = func[amount].ty.bits();
        match bits.cmp(&64) {
            std::cmp::Ordering::Less => op(func, inst, Opcode::ZExt, &[amount], WORD),
            std::cmp::Ordering::Equal => amount,
            std::cmp::Ordering::Greater => op(func, inst, Opcode::Trunc, &[amount], WORD),
        }
    }

    /// A value widened into the words of a wider one, with the sign or with zeros.
    fn extend(&self, func: &mut Func, inst: Inst, from: Value, into: Value, n: u32, signed: bool) {
        let count = words(n);
        let mut low = Vec::new();
        if let Some(&slot) = self.slots.get(&from) {
            let m = func[from].ty.bits();
            let had = words(m);
            for at in 0..had {
                let mut word = read(func, inst, slot, at);
                if at + 1 == had && !signed {
                    word = masked(func, inst, word, m);
                }
                low.push(word);
            }
        } else {
            let ty = func[from].ty;
            let m = ty.bits();
            let opcode = if signed { Opcode::SExt } else { Opcode::ZExt };
            if m <= 64 {
                let word = if m == 64 { from } else { op(func, inst, opcode, &[from], WORD) };
                low.push(word);
            } else {
                let pair = Type::int(SPLIT);
                let whole = if m == SPLIT { from } else { op(func, inst, opcode, &[from], pair) };
                let bottom = op(func, inst, Opcode::Trunc, &[whole], WORD);
                let sixty_four = constant(func, inst, pair, 64);
                let top = op(func, inst, Opcode::LShr, &[whole, sixty_four], pair);
                let top = op(func, inst, Opcode::Trunc, &[top], WORD);
                low.extend([bottom, top]);
            }
        }
        let fill = if signed {
            let last = *low.last().expect("a value has at least one word");
            let sixty_three = constant(func, inst, WORD, 63);
            op(func, inst, Opcode::AShr, &[last, sixty_three], WORD)
        } else {
            constant(func, inst, WORD, 0)
        };
        for at in 0..count {
            let mut word = low.get(at as usize).copied().unwrap_or(fill);
            if at + 1 == count {
                word = extended(func, inst, word, n);
            }
            write(func, inst, word, into, at);
        }
    }

    /// A call to a libgcc routine in front of an instruction, answering the value it returns when
    /// it returns one.
    fn call(
        &mut self,
        func: &mut Func,
        inst: Inst,
        routine: &str,
        args: &[Value],
        returns: Option<Type>,
    ) -> Option<Value> {
        let mut built = Signature::new();
        built.params = args.iter().map(|&value| Param::new(func[value].ty)).collect();
        built.returns = returns.into_iter().map(Param::new).collect();
        let signature = func.add_signature(built);
        let callee = Some(self.names.intern(routine));
        let varargs = func.push_abis(&[]);
        let extra = Extra::Call(func.add_call(CallInfo { callee, signature, varargs }));
        let args = func.push_values(args);
        let data = InstData { args, extra, ..InstData::new(Opcode::Call) };
        let results: Vec<Type> = returns.into_iter().collect();
        let span = func.span(inst);
        let made = func.create_inst(data, &results, span);
        func.insert_before(made, inst);
        func[made].first_result
    }

    /// The edges out of one terminator, with the words of each wide argument copied into the slot
    /// of the parameter it arrives as.
    fn edges(&mut self, func: &mut Func, inst: Inst) {
        let list = func.target_list(inst);
        let alone = list.len() == 1 && func[inst].opcode == Opcode::Jump;
        for at in list.iter() {
            let call = func[at];
            let args = func[call.args].to_vec();
            if !args.iter().any(|value| self.slots.contains_key(value)) {
                continue;
            }
            let params = func[call.block].params.clone();
            // The copy goes in front of the jump when the jump is the only way out. Otherwise it
            // goes in a block of its own on the edge, see the module's third section.
            let (before, split) = if alone {
                (inst, None)
            } else {
                let span = func.span(inst);
                let between = func.create_block();
                let jump = Builder::new(func, between).at(span).jump(call.block, &args);
                (jump, Some(between))
            };
            let pairs: Vec<(Value, Value, u32)> = args
                .iter()
                .zip(&params)
                .filter_map(|(arg, param)| {
                    let to = *self.slots.get(param)?;
                    let from = self.held(*arg);
                    Some((from, to, words(func[*param].ty.bits())))
                })
                .filter(|(from, to, _)| from != to)
                .collect();
            let crossed =
                pairs.iter().any(|(from, _, _)| pairs.iter().any(|(_, to, _)| from == to));
            if crossed {
                let staged: Vec<(Value, Value, u32)> = pairs
                    .iter()
                    .map(|&(from, to, count)| {
                        let stage = slot(func, count);
                        copy(func, before, from, stage, count);
                        (stage, to, count)
                    })
                    .collect();
                for (stage, to, count) in staged {
                    copy(func, before, stage, to, count);
                }
            } else {
                for &(from, to, count) in &pairs {
                    copy(func, before, from, to, count);
                }
            }
            let passed: Vec<Value> = args
                .iter()
                .zip(&params)
                .map(|(arg, param)| self.slots.get(param).copied().unwrap_or(*arg))
                .collect();
            match split {
                None => {
                    let passed = func.push_values(&passed);
                    func.set_block_call(at, BlockCall { args: passed, ..call });
                }
                Some(between) => {
                    let jump = before;
                    let list = func.target_list(jump);
                    for inner in list.iter() {
                        let inner_call = func[inner];
                        let passed = func.push_values(&passed);
                        func.set_block_call(inner, BlockCall { args: passed, ..inner_call });
                    }
                    let none = func.push_values(&[]);
                    func.set_block_call(at, BlockCall { block: between, args: none, ..call });
                }
            }
        }
    }
}

/// `-N`, the precision libgcc reads as a signed value of `N` bits.
fn signed_precision(n: u32) -> i32 {
    -i32::try_from(n).expect("a _BitInt is at most 65535 bits")
}

/// `N`, the precision libgcc reads as an unsigned value of `N` bits.
fn unsigned_precision(n: u32) -> i32 {
    i32::try_from(n).expect("a _BitInt is at most 65535 bits")
}

/// A comparison of two values kept in words, as one bit.
///
/// Equality is every word equal. An order is the walk from the bottom word up, where each word
/// decides when it differs and passes on what the words below it said when it does not. Only the
/// top word is compared signed for a signed order, and see the module's first section for why the
/// unsigned orders need no clearing first.
fn compare(func: &mut Func, inst: Inst, pred: IntPred, a: Value, b: Value, n: u32) -> Value {
    let count = words(n);
    let zero = constant(func, inst, WORD, 0);
    if matches!(pred, IntPred::Eq | IntPred::Ne) {
        let mut differ = zero;
        for at in 0..count {
            let x = read(func, inst, a, at);
            let y = read(func, inst, b, at);
            let bits = op(func, inst, Opcode::Xor, &[x, y], WORD);
            differ = op(func, inst, Opcode::Or, &[differ, bits], WORD);
        }
        return icmp(func, inst, pred, differ, zero);
    }
    let signed = matches!(pred, IntPred::Slt | IntPred::Sle | IntPred::Sgt | IntPred::Sge);
    // Every order is `less` with the operands one way round or the other, said or denied.
    let (x, y, denied) = match pred {
        IntPred::Slt | IntPred::Ult => (a, b, false),
        IntPred::Sgt | IntPred::Ugt => (b, a, false),
        IntPred::Sle | IntPred::Ule => (b, a, true),
        _ => (a, b, true),
    };
    let mut below = zero;
    for at in 0..count {
        let p = read(func, inst, x, at);
        let q = read(func, inst, y, at);
        let pred = if signed && at + 1 == count { IntPred::Slt } else { IntPred::Ult };
        let here = less(func, inst, pred, p, q);
        let same = icmp(func, inst, IntPred::Eq, p, q);
        let same = op(func, inst, Opcode::ZExt, &[same], WORD);
        let kept = op(func, inst, Opcode::And, &[same, below], WORD);
        below = op(func, inst, Opcode::Or, &[here, kept], WORD);
    }
    let pred = if denied { IntPred::Eq } else { IntPred::Ne };
    icmp(func, inst, pred, below, zero)
}

/// A shift of a value kept in words by an amount in a word, into a slot.
///
/// The words are put in a buffer with a word of fill for every word of the value and one more, and
/// each word of the answer is two neighbouring words of the buffer at an offset the amount picks,
/// shifted by what is left of the amount and joined. The offset is held to the value's length so a
/// shift by its width or more, which C leaves undefined, still reads inside the buffer.
fn shift(
    func: &mut Func,
    inst: Inst,
    opcode: Opcode,
    from: Value,
    amount: Value,
    into: Value,
    n: u32,
) {
    let count = words(n);
    let buffer = slot(func, 2 * count + 1);
    let zero = constant(func, inst, WORD, 0);
    let sixty_three = constant(func, inst, WORD, 63);
    let one = constant(func, inst, WORD, 1);
    let six = constant(func, inst, WORD, 6);
    let three = constant(func, inst, WORD, 3);
    let most = constant(func, inst, WORD, i128::from(count));
    let skip = op(func, inst, Opcode::LShr, &[amount, six], WORD);
    let over = icmp(func, inst, IntPred::Ugt, skip, most);
    let skip = op(func, inst, Opcode::Select, &[over, most, skip], WORD);
    let rest = op(func, inst, Opcode::And, &[amount, sixty_three], WORD);
    let other = op(func, inst, Opcode::Sub, &[sixty_three, rest], WORD);
    if opcode == Opcode::Shl {
        // Fill below, then the value: the word an answer reads comes from `skip` words down.
        for at in 0..=count {
            write(func, inst, zero, buffer, at);
        }
        for at in 0..count {
            let word = read(func, inst, from, at);
            write(func, inst, word, buffer, count + 1 + at);
        }
        let back = op(func, inst, Opcode::Sub, &[most, skip], WORD);
        let bytes = op(func, inst, Opcode::Shl, &[back, three], WORD);
        let base = op(func, inst, Opcode::PtrAdd, &[buffer, bytes], Type::PTR);
        for at in 0..count {
            let low = read(func, inst, base, at);
            let high = read(func, inst, base, at + 1);
            let high = op(func, inst, Opcode::Shl, &[high, rest], WORD);
            // Two shifts rather than one by `64 - rest`, which is a shift by sixty four when
            // `rest` is zero and that is not a shift the machine does.
            let low = op(func, inst, Opcode::LShr, &[low, one], WORD);
            let low = op(func, inst, Opcode::LShr, &[low, other], WORD);
            let mut word = op(func, inst, Opcode::Or, &[high, low], WORD);
            if at + 1 == count {
                word = extended(func, inst, word, n);
            }
            write(func, inst, word, into, at);
        }
    } else {
        // The value, then fill above: the word an answer reads comes from `skip` words up.
        let mut top = zero;
        for at in 0..count {
            let mut word = read(func, inst, from, at);
            if at + 1 == count {
                if opcode == Opcode::LShr {
                    word = masked(func, inst, word, n);
                }
                top = word;
            }
            write(func, inst, word, buffer, at);
        }
        let fill = if opcode == Opcode::AShr {
            op(func, inst, Opcode::AShr, &[top, sixty_three], WORD)
        } else {
            zero
        };
        for at in count..=2 * count {
            write(func, inst, fill, buffer, at);
        }
        let bytes = op(func, inst, Opcode::Shl, &[skip, three], WORD);
        let base = op(func, inst, Opcode::PtrAdd, &[buffer, bytes], Type::PTR);
        for at in 0..count {
            let low = read(func, inst, base, at);
            let high = read(func, inst, base, at + 1);
            let low = op(func, inst, Opcode::LShr, &[low, rest], WORD);
            let high = op(func, inst, Opcode::Shl, &[high, one], WORD);
            let high = op(func, inst, Opcode::Shl, &[high, other], WORD);
            let mut word = op(func, inst, Opcode::Or, &[high, low], WORD);
            if at + 1 == count {
                word = extended(func, inst, word, n);
            }
            write(func, inst, word, into, at);
        }
    }
}

/// The bottom of a value kept in words, as a narrower integer.
fn bottom(func: &mut Func, inst: Inst, from: Value, ty: Type) -> Value {
    let n = ty.bits();
    let low = read(func, inst, from, 0);
    if n <= 64 {
        return narrowed(func, inst, low, ty);
    }
    let pair = Type::int(SPLIT);
    let high = read(func, inst, from, 1);
    let low = op(func, inst, Opcode::ZExt, &[low], pair);
    let high = op(func, inst, Opcode::ZExt, &[high], pair);
    let sixty_four = constant(func, inst, pair, 64);
    let high = op(func, inst, Opcode::Shl, &[high, sixty_four], pair);
    let whole = op(func, inst, Opcode::Or, &[high, low], pair);
    if n == SPLIT { whole } else { op(func, inst, Opcode::Trunc, &[whole], ty) }
}

/// A word as a narrower integer, or as itself at sixty four bits.
fn narrowed(func: &mut Func, inst: Inst, word: Value, ty: Type) -> Value {
    if ty.bits() == 64 { word } else { op(func, inst, Opcode::Trunc, &[word], ty) }
}

/// The top word of a value `n` bits wide, sign extended from bit `n - 1`.
fn extended(func: &mut Func, inst: Inst, word: Value, n: u32) -> Value {
    let spare = (64 - n % 64) % 64;
    if spare == 0 {
        return word;
    }
    let spare = constant(func, inst, WORD, i128::from(spare));
    let up = op(func, inst, Opcode::Shl, &[word, spare], WORD);
    op(func, inst, Opcode::AShr, &[up, spare], WORD)
}

/// The top word of a value `n` bits wide, with the bits above `n` cleared.
fn masked(func: &mut Func, inst: Inst, word: Value, n: u32) -> Value {
    let used = n % 64;
    if used == 0 {
        return word;
    }
    let mask = constant(func, inst, WORD, (1i128 << used) - 1);
    op(func, inst, Opcode::And, &[word, mask], WORD)
}

/// A copy of a value kept in words with the bits above `n` cleared, which is how an unsigned value
/// is handed to libgcc. The value itself when there are no such bits.
fn cleared(func: &mut Func, inst: Inst, from: Value, n: u32) -> Value {
    if n % 64 == 0 {
        return from;
    }
    let count = words(n);
    let copy = slot(func, count);
    for at in 0..count {
        let mut word = read(func, inst, from, at);
        if at + 1 == count {
            word = masked(func, inst, word, n);
        }
        write(func, inst, word, copy, at);
    }
    copy
}

/// The top word of a slot libgcc wrote, put back into the form this pass keeps, since the bits
/// above `n` are not something a routine promises anything about.
fn settle(func: &mut Func, inst: Inst, slot: Value, n: u32) {
    if n % 64 == 0 {
        return;
    }
    let top = words(n) - 1;
    let word = read(func, inst, slot, top);
    let word = extended(func, inst, word, n);
    write(func, inst, word, slot, top);
}

/// The words of one slot copied into another.
fn copy(func: &mut Func, inst: Inst, from: Value, into: Value, count: u32) {
    for at in 0..count {
        let word = read(func, inst, from, at);
        write(func, inst, word, into, at);
    }
}

/// A comparison of two words, as a word that is zero or one.
fn less(func: &mut Func, inst: Inst, pred: IntPred, a: Value, b: Value) -> Value {
    let bit = icmp(func, inst, pred, a, b);
    op(func, inst, Opcode::ZExt, &[bit], WORD)
}

/// A comparison in front of an instruction.
fn icmp(func: &mut Func, inst: Inst, pred: IntPred, a: Value, b: Value) -> Value {
    let args = func.push_values(&[a, b]);
    let data = InstData { args, extra: Extra::IntPred(pred), ..InstData::new(Opcode::ICmp) };
    put(func, inst, data, Type::I1)
}

/// A slot of `count` words at the top of the entry block.
fn slot(func: &mut Func, count: u32) -> Value {
    let entry = func.entry().expect("a function with blocks has an entry");
    let inst = func[entry].first.expect("the entry block of a function this wide is not empty");
    let info = MemInfo { size: u64::from(count) * 8, align: 16, ..access(8) };
    let extra = Extra::Mem(func.add_mem(info));
    put(func, inst, InstData { extra, ..InstData::new(Opcode::Alloca) }, Type::PTR)
}

/// Word `at` of a slot.
fn read(func: &mut Func, inst: Inst, slot: Value, at: u32) -> Value {
    let address = offset(func, inst, slot, u64::from(at) * 8);
    load(func, inst, address, 8)
}

/// A word written as word `at` of a slot.
fn write(func: &mut Func, inst: Inst, word: Value, slot: Value, at: u32) {
    let address = offset(func, inst, slot, u64::from(at) * 8);
    store(func, inst, word, address, 8);
}

/// An address that many bytes past another one.
fn offset(func: &mut Func, inst: Inst, base: Value, bytes: u64) -> Value {
    if bytes == 0 {
        return base;
    }
    let bytes = constant(func, inst, WORD, i128::from(bytes));
    op(func, inst, Opcode::PtrAdd, &[base, bytes], Type::PTR)
}

/// A word read from an address.
fn load(func: &mut Func, inst: Inst, address: Value, align: u32) -> Value {
    let extra = Extra::Mem(func.add_mem(access(align)));
    let args = func.push_values(&[address]);
    put(func, inst, InstData { args, extra, ..InstData::new(Opcode::Load) }, WORD)
}

/// A word written to an address.
fn store(func: &mut Func, inst: Inst, word: Value, address: Value, align: u32) {
    let extra = Extra::Mem(func.add_mem(access(align)));
    let args = func.push_values(&[word, address]);
    let data = InstData { args, extra, ..InstData::new(Opcode::Store) };
    let span = func.span(inst);
    let made = func.create_inst(data, &[], span);
    func.insert_before(made, inst);
}

/// A plain access to one word.
fn access(align: u32) -> MemInfo {
    MemInfo {
        size: 8,
        align,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    }
}

/// An integer constant in front of an instruction.
fn constant(func: &mut Func, inst: Inst, ty: Type, value: i128) -> Value {
    let imm = func.add_imm(Imm::int(value, ty));
    put(func, inst, InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) }, ty)
}

/// An instruction of that opcode over those operands in front of another one.
fn op(func: &mut Func, inst: Inst, opcode: Opcode, args: &[Value], ty: Type) -> Value {
    let args = func.push_values(args);
    let flags = Flags::NONE;
    put(func, inst, InstData { args, flags, ..InstData::new(opcode) }, ty)
}

/// Creates an instruction with one result, puts it in front of another one, and answers the
/// result.
fn put(func: &mut Func, inst: Inst, data: InstData, ty: Type) -> Value {
    let span = func.span(inst);
    let made = func.create_inst(data, &[ty], span);
    func.insert_before(made, inst);
    func[made].first_result.expect("an instruction created with one result has one")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Def, Extra, Flags, Func, IntPred, MemInfo, MemOrder, Module, Opcode,
        Restrict, Signature, Type, Value,
    };
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::{SPLIT, limbs};

    /// The width every test here works at, which is gcc's `_BitInt(200)`: four words with the top
    /// one partly used.
    const WIDE: Type = Type::int(200);

    /// An access to a whole `_BitInt(200)` in memory, the way the front end writes one.
    fn whole() -> MemInfo {
        MemInfo {
            size: 32,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        }
    }

    /// A function of three addresses with the entry block to fill.
    fn three() -> (Interner, Func, Block, Vec<Value>) {
        let mut names = Interner::new();
        let params = [Type::PTR, Type::PTR, Type::PTR];
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&params));
        let entry = func.create_block();
        let args = params.iter().map(|&ty| func.append_param(entry, ty)).collect();
        (names, func, entry, args)
    }

    /// The pass run on a function, which then has to be valid IR with no value wider than a
    /// hundred and twenty eight bits read or written anywhere in it.
    fn rewritten(names: &mut Interner, func: &mut Func) {
        assert!(limbs(func, names, 8), "the function was left alone");
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let module = Module::new(names.intern("f.c"), &target);
        rucc_ir::verify_func(&module, func, names).expect("the rewrite builds valid IR");
        let blocks: Vec<Block> = func.blocks().collect();
        for block in blocks {
            for &param in &func[block].params {
                assert!(func[param].ty.bits() <= SPLIT, "a wide parameter is left");
            }
            for inst in func.insts(block) {
                let data = func[inst];
                for &arg in &func[data.args] {
                    assert!(
                        func[arg].ty.bits() <= SPLIT,
                        "{} reads a wide value",
                        data.opcode.name()
                    );
                }
                for result in data.results() {
                    assert!(func[result].ty.bits() <= SPLIT, "{} is wide", data.opcode.name());
                }
            }
        }
    }

    /// The routines a function calls, by name.
    fn called(func: &Func, names: &Interner) -> Vec<String> {
        let mut found = Vec::new();
        for block in func.blocks() {
            for inst in func.insts(block) {
                let Extra::Call(info) = func[inst].extra else { continue };
                if let Some(callee) = func[info].callee {
                    found.push(names.resolve(callee).to_owned());
                }
            }
        }
        found
    }

    /// The precisions each call passes, in the order it passes them, as `i32` constants.
    fn precisions(func: &Func) -> Vec<Vec<i128>> {
        let mut found = Vec::new();
        for block in func.blocks() {
            for inst in func.insts(block) {
                if !matches!(func[inst].extra, Extra::Call(_)) {
                    continue;
                }
                let constants = func[func[inst].args]
                    .iter()
                    .filter(|&&arg| func[arg].ty == Type::int(32))
                    .filter_map(|&arg| {
                        let Def::Result { inst, .. } = func[arg].def else { return None };
                        let Extra::Imm(imm) = func[inst].extra else { return None };
                        Some(func[imm].signed(Type::int(32)))
                    })
                    .collect();
                found.push(constants);
            }
        }
        found
    }

    #[test]
    fn a_wide_sum_and_shift_are_words_in_the_frame() {
        let (mut names, mut func, entry, args) = three();
        let mut build = Builder::new(&mut func, entry);
        let a = build.load(WIDE, args[0], whole(), Flags::NONE);
        let b = build.load(WIDE, args[1], whole(), Flags::NONE);
        let sum = build.binary(Opcode::Add, a, b, Flags::NONE);
        let less = build.binary(Opcode::Sub, sum, b, Flags::NONE);
        let three = build.iconst(WIDE, 3);
        let up = build.binary(Opcode::Shl, less, three, Flags::NONE);
        let down = build.binary(Opcode::AShr, up, b, Flags::NONE);
        let across = build.binary(Opcode::LShr, down, a, Flags::NONE);
        let mixed = build.binary(Opcode::Xor, across, a, Flags::NONE);
        build.store(mixed, args[2], whole(), Flags::NONE);
        build.ret(&[]);
        rewritten(&mut names, &mut func);
        assert!(called(&func, &names).is_empty());
    }

    #[test]
    fn a_wide_product_quotient_and_conversion_are_libgcc_calls() {
        let (mut names, mut func, entry, args) = three();
        let f64 = Type::float(rucc_ir::Float::F64);
        let mut build = Builder::new(&mut func, entry);
        let a = build.load(WIDE, args[0], whole(), Flags::NONE);
        let b = build.load(WIDE, args[1], whole(), Flags::NONE);
        let product = build.binary(Opcode::Mul, a, b, Flags::NONE);
        let quotient = build.binary(Opcode::UDiv, product, b, Flags::NONE);
        let left = build.binary(Opcode::SRem, quotient, a, Flags::NONE);
        let float = build.unary(Opcode::SIToFP, left, f64);
        let back = build.unary(Opcode::FPToUI, float, WIDE);
        build.store(back, args[2], whole(), Flags::NONE);
        build.ret(&[]);
        rewritten(&mut names, &mut func);
        assert_eq!(
            called(&func, &names),
            [
                "__mulbitint3",
                "__divmodbitint4",
                "__divmodbitint4",
                "__floatbitintdf",
                "__fixdfbitint"
            ]
        );
        // What libgcc writes is `N` bits whatever the signedness, as gcc passes it, and what it
        // reads is `-N` for a signed value. A negative output precision makes `__mulbitint3`
        // write past the end of its words.
        assert_eq!(
            precisions(&func),
            [
                vec![200, -200, -200],
                vec![200, 0, 200, 200],
                vec![0, 200, -200, -200],
                vec![-200],
                vec![200],
            ]
        );
    }

    #[test]
    fn a_wide_value_crosses_an_edge_through_the_slot_of_the_parameter() {
        let (mut names, mut func, entry, args) = three();
        let then = func.create_block();
        let other = func.create_block();
        let join = func.create_block();
        let joined = func.append_param(join, WIDE);
        let mut build = Builder::new(&mut func, entry);
        let a = build.load(WIDE, args[0], whole(), Flags::NONE);
        let b = build.load(WIDE, args[1], whole(), Flags::NONE);
        let less = build.icmp(IntPred::Slt, a, b);
        build.br_if(less, then, &[], other, &[]);
        let mut build = Builder::new(&mut func, then);
        let narrow = build.unary(Opcode::Trunc, a, Type::int(32));
        let wider = build.unary(Opcode::SExt, narrow, WIDE);
        build.jump(join, &[wider]);
        Builder::new(&mut func, other).jump(join, &[b]);
        let mut build = Builder::new(&mut func, join);
        build.store(joined, args[2], whole(), Flags::NONE);
        build.ret(&[]);
        rewritten(&mut names, &mut func);
        assert_eq!(func[joined].ty, Type::PTR);
    }

    #[test]
    fn two_wide_parameters_passed_crosswise_are_swapped() {
        let (mut names, mut func, entry, args) = three();
        let head = func.create_block();
        let done = func.create_block();
        let first = func.append_param(head, WIDE);
        let second = func.append_param(head, WIDE);
        let mut build = Builder::new(&mut func, entry);
        let a = build.load(WIDE, args[0], whole(), Flags::NONE);
        let b = build.load(WIDE, args[1], whole(), Flags::NONE);
        build.jump(head, &[a, b]);
        let mut build = Builder::new(&mut func, head);
        let same = build.icmp(IntPred::Ule, first, second);
        build.br_if(same, head, &[second, first], done, &[]);
        let mut build = Builder::new(&mut func, done);
        build.store(first, args[2], whole(), Flags::NONE);
        build.ret(&[]);
        let before = func.blocks().count();
        rewritten(&mut names, &mut func);
        // The way back to the head has a block of its own for the copies.
        assert_eq!(func.blocks().count(), before + 1);
    }

    #[test]
    fn a_function_with_an_overflow_check_this_wide_is_left_alone() {
        let (mut names, mut func, entry, args) = three();
        let mut build = Builder::new(&mut func, entry);
        let a = build.load(WIDE, args[0], whole(), Flags::NONE);
        let (sum, _) = build.checked(Opcode::SAddOverflow, a, a);
        build.store(sum, args[2], whole(), Flags::NONE);
        build.ret(&[]);
        assert!(!limbs(&mut func, &mut names, 8));
        assert_eq!(func[a].ty, WIDE);
    }

    #[test]
    fn a_function_with_nothing_this_wide_or_on_a_narrow_machine_is_left_alone() {
        let (mut names, mut func, entry, args) = three();
        let mut build = Builder::new(&mut func, entry);
        let a = build.load(Type::int(128), args[0], whole(), Flags::NONE);
        build.store(a, args[2], whole(), Flags::NONE);
        build.ret(&[]);
        assert!(!limbs(&mut func, &mut names, 8));

        let (mut names, mut func, entry, args) = three();
        let mut build = Builder::new(&mut func, entry);
        let a = build.load(WIDE, args[0], whole(), Flags::NONE);
        build.store(a, args[2], whole(), Flags::NONE);
        build.ret(&[]);
        assert!(!limbs(&mut func, &mut names, 4));
    }
}
