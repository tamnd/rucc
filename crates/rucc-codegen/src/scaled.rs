//! A scaled index written again in each block that reads it, on x86, so that each address can take
//! the index and its scale in the one memory operand.
//!
//! `box->list[i]` and `assign[i]` in one loop are `i << 2` added to two pointers, and the optimizer
//! keeps one shift for both. Selection folds an instruction into its readers only when they are in
//! its block and every one of them takes it, see `Lowering::foldable` in [`crate::lower`], so a
//! shift that a later block reads too stays an instruction, and its result is one more value held
//! across the branch:
//!
//! ```text
//!   shll $2, %edi                     movl 64(%eax,%esi,4), %eax
//!   movl 64(%eax,%edi), %eax    ->    ...
//!   ...                               movl (%ecx,%esi,4), %edx
//!   movl (%ecx,%edi), %edx
//! ```
//!
//! gcc writes the right hand side. So a shift by one, two or three that is read by nothing but
//! the offset of a `ptr_add`, which is a scale an x86 address has room for, is written again in
//! front of the first such reader in each other block, and the readers there read the copy. Each
//! block then has a shift all of whose readers are beside it, which is the shape selection folds.
//! What the copy reads was there before the shift, so it is there in every block the shift reached.
//!
//! The index register is what is live across the blocks instead of the shifted one. Where the
//! index is read again, which in a loop it is, that is one value fewer, and otherwise it is one for
//! one with a shift saved.

use rucc_base::hash::Map;
use rucc_ir::{Block, Def, Extra, Func, Imm, Inst, InstData, Opcode, Value};

/// Writes each such shift again in the other blocks that read it, and gives back how many shifts it
/// did that for.
///
/// `word` is the size of an address in bytes.
///
/// # Panics
///
/// If a constant or a shift it makes has no result, which neither can fail to have.
pub fn indexes(func: &mut Func, word: u32) -> usize {
    let mut readers: Map<Value, Vec<(Inst, Option<usize>)>> = Map::default();
    let mut shifts = Vec::new();
    for block in func.blocks().collect::<Vec<Block>>() {
        for inst in func.insts(block).collect::<Vec<Inst>>() {
            for (at, &arg) in func[func[inst].args].iter().enumerate() {
                readers.entry(arg).or_default().push((inst, Some(at)));
            }
            for call in func.successors(inst).collect::<Vec<_>>() {
                for &arg in &func[call.args] {
                    readers.entry(arg).or_default().push((inst, None));
                }
            }
            if func[inst].opcode == Opcode::Shl {
                shifts.push(inst);
            }
        }
    }
    let mut written = 0;
    for shift in shifts {
        let Some(home) = func.block_of(shift) else { continue };
        let Some(value) = func[shift].first_result else { continue };
        let &[index, by] = &func[func[shift].args] else { continue };
        let Some(amount) = amount(func, by) else { continue };
        let Some(read) = readers.get(&value) else { continue };
        let offset = |&(reader, at): &(Inst, Option<usize>)| {
            func[reader].opcode == Opcode::PtrAdd
                && at == Some(1)
                && func[func[reader].args][0] != value
        };
        if func[value].ty.bits() != word * 8
            || !(1..=3).contains(&amount)
            || !read.iter().all(offset)
            || read.iter().all(|&(reader, _)| func.block_of(reader) == Some(home))
        {
            continue;
        }
        let mut elsewhere: Vec<(Block, Vec<Inst>)> = Vec::new();
        for &(reader, _) in read {
            let Some(block) = func.block_of(reader) else { continue };
            if block == home {
                continue;
            }
            match elsewhere.iter_mut().find(|(at, _)| *at == block) {
                Some((_, list)) => list.push(reader),
                None => elsewhere.push((block, vec![reader])),
            }
        }
        let ty = func[value].ty;
        let flags = func[shift].flags;
        let span = func.span(shift);
        for (block, list) in elsewhere {
            let Some(first) = func.insts(block).find(|inst| list.contains(inst)) else { continue };
            let imm = func.add_imm(Imm::int(i128::from(amount), func[by].ty.lane()));
            let made = func.create_inst(
                InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) },
                &[func[by].ty],
                span,
            );
            func.insert_before(made, first);
            let constant = func[made].first_result.expect("a constant has a result");
            let args = func.push_values(&[index, constant]);
            let made = func.create_inst(
                InstData { args, flags, ..InstData::new(Opcode::Shl) },
                &[ty],
                span,
            );
            func.insert_before(made, first);
            let copy = func[made].first_result.expect("a shift has a result");
            for reader in list {
                let args = func[reader].args;
                func.rewrite(args, |arg| if arg == value { copy } else { arg });
            }
        }
        if read.iter().all(|&(reader, _)| func.block_of(reader) != Some(home)) {
            func.remove_inst(shift);
        }
        written += 1;
    }
    written
}

/// The number a shift is by, when it is a constant.
fn amount(func: &Func, value: Value) -> Option<u32> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    let Extra::Imm(imm) = func[inst].extra else { return None };
    if func[inst].opcode != Opcode::IConst {
        return None;
    }
    u32::try_from(func[imm].unsigned()).ok()
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::*;

    /// `a[i]` read in the first block and `b[i]` in the second, with one shift for both.
    const TWO_BLOCKS: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "i686-unknown-linux-gnu"
target datalayout = "e-p:32:32-i64:32-f80:128-S128"

func @pick(ptr, ptr, i32) -> i32, linkage(external) {
block0(%0: ptr, %1: ptr, %2: i32):
    %3 = iconst.i32 2
    %4 = shl %2, %3
    %5 = ptr_add %0, %4
    %6 = load.i32 %5, align 4
    %7 = iconst.i32 0
    %8 = icmp ne %6, %7
    br_if %8, block1, block2

block1:
    %9 = ptr_add %1, %4
    %10 = load.i32 %9, align 4
    return %10

block2:
    return %6
}
"#;

    /// The fixture after the pass, as text, once it is checked, and what the pass returned.
    fn scaled(source: &str) -> (String, usize) {
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(source, &mut names).expect("the fixture is IR");
        let id = module.funcs().last().expect("the fixture has a function");
        let written = indexes(&mut module[id], 4);
        let func = &module[id];
        rucc_ir::verify_func(&module, func, &names).expect("the rewrite builds valid IR");
        (rucc_ir::print_func(&module, func, &names), written)
    }

    /// The lines of the block with that label, up to the next blank line.
    fn block<'a>(text: &'a str, label: &str) -> Vec<&'a str> {
        let lines = text.lines().skip_while(|line| !line.starts_with(label));
        lines.take_while(|line| !line.is_empty()).collect()
    }

    #[test]
    fn a_shift_another_block_reads_as_an_offset_is_written_again_there() {
        let (text, written) = scaled(TWO_BLOCKS);
        assert_eq!(written, 1, "{text}");
        assert_eq!(text.matches("= shl").count(), 2, "{text}");
        for label in ["block0", "block1"] {
            let lines = block(&text, label);
            let shift = lines.iter().find(|line| line.contains("= shl")).expect("a shift");
            let made = shift.trim().split(' ').next().expect("the shift has a name");
            let add = lines.iter().find(|line| line.contains("ptr_add")).expect("an address");
            assert!(add.ends_with(&format!(", {made}")), "{label} reads its own shift\n{text}");
        }
    }

    #[test]
    fn a_shift_read_as_a_number_somewhere_is_left_alone() {
        let source = TWO_BLOCKS.replace("return %10", "%11 = add %10, %4\n    return %11");
        let (text, written) = scaled(&source);
        assert_eq!(written, 0, "{text}");
        assert_eq!(text.matches("= shl").count(), 1, "{text}");
    }

    #[test]
    fn a_shift_too_wide_for_an_address_is_left_alone() {
        let (text, written) = scaled(&TWO_BLOCKS.replace("iconst.i32 2", "iconst.i32 4"));
        assert_eq!(written, 0, "{text}");
        assert_eq!(text.matches("= shl").count(), 1, "{text}");
    }
}
