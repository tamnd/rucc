//! A subscript that is a narrow sum with a constant in it, `a[i - 1]`, is read as the wide index
//! with the constant moved out into the address.
//!
//! C adds the `int`s first and widens the sum, so `a[i - 1]` is `a + 8 * sext(i - 1)`, and on
//! x86-64 that is a subtraction, a `movslq` and the load. The constant is inside the extension and
//! the address can only take it as a displacement from outside. gcc and clang write `movslq` and a
//! load at `-8(%rdi,%rax,8)`, and where the program also reads `a[i]` the two share the one
//! `movslq`. Postgres writes it everywhere it has an attribute number, as `attnum - 1`, and in the
//! loop of `_bt_compare` the index into the descriptor's attributes was the subtraction and its
//! own extension on every key. See tamnd/rucc#1994.
//!
//! The rewrite is `base + scale * sext(x + c)` to `(base + scale * sext(x)) + scale * c`, and the
//! second `ptr_add` is one a load or a store folds into its displacement. It is only right where
//! the narrow sum does not wrap, which is what `nsw` on it says, since then `sext(x + c)` is
//! `sext(x) + c` exactly. A sum without the flag, as under `-fwrapv`, is left alone. The scaling
//! and the two additions of the address are arithmetic modulo the width of a pointer, where they
//! distribute however they are grouped, so the address comes out the same number.
//!
//! Only where every reader of the scaled index is an address built from it, so that the old
//! extension has nothing left reading it afterwards and the rewrite adds no instruction it does not
//! take one away for. The new
//! extension of `x` is built where the old one was, and `crate::number` after this pass finds it the
//! same as any other extension of `x` the function already had.

use rucc_ir::{Def, Extra, Flags, Func, Imm, Inst, InstData, Opcode, Type, Value};

use crate::uses::count;
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// What this pass is called, for the lists in [`crate::pipeline`] that name it.
pub const NAME: &str = "subscript";

const MOVED: &str = "constant of a narrow subscript moved out into the address";
const OUT_OF_FUEL: &str = "constant left in the subscript, the fuel for this compilation ran out";

/// The most a constant moved out may be, which is what a displacement on x86-64 holds. One larger
/// than that would be an addition of its own in front of the address, which is no better than the
/// one inside the subscript was.
const DISPLACEMENT: i128 = 1 << 31;

/// The pass.
#[derive(Debug)]
pub struct Subscript;

impl Pass for Subscript {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "moves the constant of a narrow subscript out of its widening and into the address"
    }

    fn preserves(&self) -> Preserved {
        // Instructions come and go inside blocks and no edge moves.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, _: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let uses = count(func);
        // How many times each value is read as the offset of an address, which is the reader every
        // use of the scaled index has to be for the old one to go.
        let mut offsets = vec![0u32; uses.len()];
        let mut found = Vec::new();
        for block in func.blocks() {
            for inst in func.insts(block) {
                if func[inst].opcode != Opcode::PtrAdd {
                    continue;
                }
                if let &[base, by] = &func[func[inst].args] {
                    if base != by {
                        offsets[by.index()] += 1;
                        found.push(inst);
                    }
                }
            }
        }
        // One rebuilt index per scaled one, however many addresses read it.
        let mut rebuilt: Vec<(Value, Value)> = Vec::new();
        for inst in found {
            let by = func[func[inst].args][1];
            let Some(shape) = shape(func, by) else { continue };
            if uses[by.index()] != offsets[by.index()]
                || shape.scaled.is_some_and(|_| uses[shape.extension.index()] != 1)
            {
                continue;
            }
            let Some(moved) = shape.constant.checked_mul(shape.factor) else { continue };
            if !(-DISPLACEMENT..DISPLACEMENT).contains(&moved) {
                continue;
            }
            if !fuel.take() {
                stats.missed(OUT_OF_FUEL);
                continue;
            }
            let index = match rebuilt.iter().find(|&&(old, _)| old == by) {
                Some(&(_, index)) => index,
                None => {
                    let index = rebuild(func, by, &shape);
                    rebuilt.push((by, index));
                    index
                }
            };
            moved_out(func, inst, index, moved);
            stats.optimized(MOVED);
        }
        stats
    }
}

/// An offset that is a scaled widening of a narrow sum with a constant.
#[derive(Clone, Copy, Debug)]
struct Shape {
    /// The widening, `sext(x + c)`.
    extension: Value,
    /// What was added to, `x`.
    narrow: Value,
    /// The number added, `c`.
    constant: i128,
    /// What the widening is then multiplied or shifted by, as the opcode and its constant operand,
    /// or nothing for a subscript of bytes.
    scaled: Option<(Opcode, Value)>,
    /// What the widening is multiplied by in all.
    factor: i128,
}

/// The shape an offset has, if it is one this pass rewrites.
fn shape(func: &Func, by: Value) -> Option<Shape> {
    let (extension, scaled, factor) = match producer(func, by)? {
        (Opcode::SExt, _) => (by, None, 1),
        (Opcode::Shl, inst) => {
            let &[extension, by] = &func[func[inst].args] else { return None };
            let shift = number(func, by)?;
            if !(0..62).contains(&shift) {
                return None;
            }
            (extension, Some((Opcode::Shl, by)), 1i128 << shift)
        }
        (Opcode::Mul, inst) => {
            let &[extension, times] = &func[func[inst].args] else { return None };
            (extension, Some((Opcode::Mul, times)), number(func, times)?)
        }
        _ => return None,
    };
    let (opcode, inst) = producer(func, extension)?;
    if opcode != Opcode::SExt {
        return None;
    }
    let &[sum] = &func[func[inst].args] else { return None };
    let (opcode, inst) = producer(func, sum)?;
    if !func[inst].flags.contains(Flags::NSW) {
        return None;
    }
    let &[narrow, added] = &func[func[inst].args] else { return None };
    let constant = match opcode {
        Opcode::Add => number(func, added)?,
        Opcode::Sub => -number(func, added)?,
        _ => return None,
    };
    Some(Shape { extension, narrow, constant, scaled, factor })
}

/// The opcode of the instruction that computed a value, and the instruction.
fn producer(func: &Func, value: Value) -> Option<(Opcode, Inst)> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    Some((func[inst].opcode, inst))
}

/// The integer a value is, read as signed at its own width, where it is a constant.
fn number(func: &Func, value: Value) -> Option<i128> {
    crate::fold::constant(func, value).map(|(imm, ty)| imm.signed(ty))
}

/// The index without the constant, `scale * sext(x)`, built where the old one was.
fn rebuild(func: &mut Func, by: Value, shape: &Shape) -> Value {
    let Def::Result { inst: at, .. } = func[by].def else {
        unreachable!("the offset was matched as the result of an instruction")
    };
    let wide = func[by].ty;
    let widened = made(func, at, InstData::new(Opcode::SExt), &[shape.narrow], wide);
    let Some((opcode, operand)) = shape.scaled else { return widened };
    // The widened `x` times the factor fits the wide type whenever the narrow type is small
    // enough for it, and saying so keeps what scalar evolution can say about the address.
    let bits = func[shape.narrow].ty.bits();
    let largest = (1i128 << (bits - 1)).checked_mul(shape.factor.abs());
    let flags = if largest.is_some_and(|largest| largest < 1i128 << (wide.bits() - 1)) {
        Flags::NSW
    } else {
        Flags::NONE
    };
    let data = InstData { flags, ..InstData::new(opcode) };
    made(func, at, data, &[widened, operand], wide)
}

/// The address `base + index + moved`, written over the instruction that was `base + by`.
fn moved_out(func: &mut Func, inst: Inst, index: Value, moved: i128) {
    let base = func[func[inst].args][0];
    let wide = func[index].ty;
    let inner = made(func, inst, InstData::new(Opcode::PtrAdd), &[base, index], Type::PTR);
    let imm = func.add_imm(Imm::int(moved, wide));
    let data = InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) };
    let constant = made(func, inst, data, &[], wide);
    // A step back from the base by a negative number is a wrap read unsigned, so the flag that
    // says it is not one does not come over.
    let flags = func[inst].flags.without(Flags::NUW);
    let args = func.push_values(&[inner, constant]);
    func[inst].args = args;
    func[inst].flags = flags;
}

/// One instruction with one result and these operands, put in front of another one.
fn made(func: &mut Func, before: Inst, mut data: InstData, args: &[Value], ty: Type) -> Value {
    data.args = func.push_values(args);
    let span = func.span(before);
    let inst = func.create_inst(data, &[ty], span);
    func.insert_before(inst, before);
    func[inst].first_result.expect("one result was asked for")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::Subscript;
    use crate::{Fuel, Pass};

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    /// The module that text is, with the pass run over every function in it and what it left
    /// unread taken out.
    fn run(body: &str) -> String {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let ids: Vec<_> = module.funcs().collect();
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses();
            Subscript.run(&mut module[id], &mut an, &mut Fuel::unlimited());
            crate::dce::Dce.run(&mut module[id], &mut an, &mut Fuel::unlimited());
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    fn count(out: &str, opcode: &str) -> usize {
        out.lines().filter(|line| line.contains(&format!("= {opcode}"))).count()
    }

    /// `long f(long *a, int i) { return a[i + c]; }` with the sum carrying `flags`.
    fn subscript(c: i32, flags: &str) -> String {
        format!(
            r#"
func @f(ptr, i32) -> i64, linkage(external) {{
block0(%0: ptr, %1: i32):
    %2 = iconst.i32 {c}
    %3 = add{flags} %1, %2
    %4 = sext.i64 %3
    %5 = iconst.i64 3
    %6 = shl.nsw %4, %5
    %7 = ptr_add %0, %6
    %8 = load.i64 %7, align 8
    return %8
}}
"#
        )
    }

    /// `a[i - 1]` reads `a + 8 * sext(i)` and then eight bytes back, and nothing adds to `i`.
    #[test]
    fn the_constant_of_a_subscript_goes_into_the_address() {
        let out = run(&subscript(-1, ".nsw"));
        assert_eq!(count(&out, "add"), 0, "{out}");
        assert_eq!(count(&out, "ptr_add"), 2, "{out}");
        assert!(out.contains("iconst.i64 -8"), "{out}");
        assert!(out.contains("= sext.i64 %1"), "{out}");
    }

    /// Without `nsw` the sum may wrap, as under `-fwrapv`, and the widening of what wrapped is not
    /// the widening of `i` plus one.
    #[test]
    fn a_sum_that_may_wrap_is_left_alone() {
        let out = run(&subscript(1, ""));
        assert_eq!(count(&out, "add"), 1, "{out}");
        assert_eq!(count(&out, "ptr_add"), 1, "{out}");
    }

    /// `a[i + 2] + a[i]` widens `i` for both, and the first is the second's address and sixteen.
    #[test]
    fn a_subscript_of_bytes_and_a_multiply_are_read_the_same_way() {
        let out = run(r#"
func @f(ptr, i32) -> i32, linkage(external) {
block0(%0: ptr, %1: i32):
    %2 = iconst.i32 7
    %3 = sub.nsw %1, %2
    %4 = sext.i64 %3
    %5 = ptr_add %0, %4
    %6 = load.i8 %5, align 1
    %7 = iconst.i32 2
    %8 = add.nsw %1, %7
    %9 = sext.i64 %8
    %10 = iconst.i64 12
    %11 = mul.nsw %9, %10
    %12 = ptr_add %0, %11
    %13 = load.i32 %12, align 4
    %14 = sext.i32 %6
    %15 = add %13, %14
    return %15
}
"#);
        assert!(out.contains("iconst.i64 -7"), "{out}");
        assert!(out.contains("iconst.i64 24"), "{out}");
        assert_eq!(count(&out, "sub"), 0, "{out}");
        assert_eq!(count(&out, "ptr_add"), 4, "{out}");
    }

    /// A scaled index something other than an address reads stays, since the sum would have to
    /// stay for it anyway.
    #[test]
    fn an_index_read_for_more_than_an_address_is_left_alone() {
        let out = run(r#"
func @use(i64), linkage(external);

func @f(ptr, i32) -> i64, linkage(external) {
block0(%0: ptr, %1: i32):
    %2 = iconst.i32 1
    %3 = add.nsw %1, %2
    %4 = sext.i64 %3
    call @use(%4) : (i64)
    %5 = iconst.i64 3
    %6 = shl.nsw %4, %5
    %7 = ptr_add %0, %6
    %8 = load.i64 %7, align 8
    return %8
}
"#);
        assert_eq!(count(&out, "ptr_add"), 1, "{out}");
        assert_eq!(count(&out, "add.nsw"), 1, "{out}");
    }
}
