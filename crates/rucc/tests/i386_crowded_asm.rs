//! An i386 asm input written `"rm"` when the other operands have taken every register. The kernel's
//! 32 bit `hv_do_hypercall` pins `edx:eax`, `ecx`, `ebx`, `edi` and `esi` and hands the call target
//! over as `"rm"`, and the allocator stopped with "an instruction naming every register of its
//! class at once". Nine units of an allmodconfig build hit it, dell-smm-hwmon with an output
//! written `"=mr"` the same way. The operand now goes in a slot of the frame, as gcc puts it, and
//! one with a register left for it still gets the register.

use std::process::Command;

const CROWDED: &str = "\
typedef unsigned long long u64; typedef unsigned int u32;
extern void *pg;
register unsigned long current_stack_pointer asm(\"esp\");
u64 call(u64 control, u32 ihi, u32 ilo, u32 ohi, u32 olo)
{
  u64 st;
  if (!pg) return ~0ULL;
  __asm__ __volatile__(\"call *%[t]\"
    : \"=A\"(st), \"+c\"(ilo), \"+r\"(current_stack_pointer)
    : \"A\"(control), \"b\"(ihi), \"D\"(ohi), \"S\"(olo), [t] \"rm\"(pg)
    : \"cc\", \"memory\");
  return st;
}
";

const ROOMY: &str = "\
extern void *pg;
register unsigned long current_stack_pointer asm(\"esp\");
unsigned call(unsigned in)
{
  unsigned out;
  __asm__ __volatile__(\"call *%[t]\"
    : \"=a\"(out), \"+r\"(current_stack_pointer)
    : \"a\"(in), [t] \"rm\"(pg)
    : \"cc\", \"memory\");
  return out;
}
";

/// dell-smm-hwmon's `i8k_smm_func`, whose carry is an output written `"=mr"` while the six
/// registers hold the call.
const OUTPUT: &str = "\
struct smm_regs { unsigned int eax, ebx, ecx, edx, esi, edi; };
int smm(struct smm_regs *regs)
{
  unsigned char carry;
  asm volatile(\"out %%al,$0xb2\\n\\tout %%al,$0x84\\n\\tsetc %0\\n\"
    : \"=mr\"(carry), \"+a\"(regs->eax), \"+b\"(regs->ebx), \"+c\"(regs->ecx),
      \"+d\"(regs->edx), \"+S\"(regs->esi), \"+D\"(regs->edi));
  return carry ? -1 : 0;
}
";

/// libgcc's soft-fp `__FP_FRAC_SUB_4` for i386, from config/i386/32/sfp-machine.h: four outputs,
/// three of them written early, and four inputs that may be in a register or in memory. That is
/// eight registers against the four the allocator hands out, and addtf3.c, subtf3.c and divtf3.c
/// stopped in the allocator. gcc reads the four `"g"` inputs from the frame.
const FOUR_WORDS: &str = "\
typedef unsigned int USItype;
void sub4(USItype *r, const USItype *x, const USItype *y)
{
  USItype r3, r2, r1, r0;
  __asm__ (\"sub{l} {%11,%3|%3,%11}\\n\\tsbb{l} {%9,%2|%2,%9}\\n\\t\"
           \"sbb{l} {%7,%1|%1,%7}\\n\\tsbb{l} {%5,%0|%0,%5}\"
    : \"=r\" ((USItype) (r3)), \"=&r\" ((USItype) (r2)), \"=&r\" ((USItype) (r1)),
      \"=&r\" ((USItype) (r0))
    : \"0\" ((USItype) (x[3])), \"g\" ((USItype) (y[3])), \"1\" ((USItype) (x[2])),
      \"g\" ((USItype) (y[2])), \"2\" ((USItype) (x[1])), \"g\" ((USItype) (y[1])),
      \"3\" ((USItype) (x[0])), \"g\" ((USItype) (y[0])));
  r[0] = r0; r[1] = r1; r[2] = r2; r[3] = r3;
}
";

fn assembly(name: &str, source: &str, level: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("rucc-i386-crowded-{}-{name}{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-mregparm=3", "-fno-omit-frame-pointer"])
        .args(["-fno-pic", "-S", "-o", "-", level])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{name} {level}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

#[test]
fn an_input_with_no_register_left_goes_in_the_frame() {
    for level in ["-O0", "-O1", "-O2", "-Os"] {
        let text = assembly("crowded", CROWDED, level);
        let call = text.lines().find(|line| line.contains("call *")).expect("the asm is written");
        assert!(call.contains("(%esp)") || call.contains("(%ebp)"), "{level}: {call}\n{text}");
        for reg in ["%eax", "%ecx", "%ebx", "%esi", "%edi"] {
            assert!(!call.contains(reg), "{level}: {call}");
        }
    }
}

#[test]
fn an_output_with_no_register_left_goes_in_the_frame() {
    for level in ["-O0", "-O1", "-O2", "-Os"] {
        let text = assembly("output", OUTPUT, level);
        let set = text.lines().find(|line| line.contains("setc")).expect("the asm is written");
        assert!(set.contains("(%esp)") || set.contains("(%ebp)"), "{level}: {set}\n{text}");
    }
}

#[test]
fn an_input_with_a_register_left_keeps_it() {
    for level in ["-O1", "-O2"] {
        let text = assembly("roomy", ROOMY, level);
        let call = text.lines().find(|line| line.contains("call *")).expect("the asm is written");
        assert!(!call.contains("(%"), "{level}: {call}\n{text}");
    }
}

#[test]
fn as_many_inputs_go_in_the_frame_as_there_are_no_registers_for() {
    for level in ["-O0", "-O1", "-O2", "-Os"] {
        let text = assembly("four", FOUR_WORDS, level);
        let all: Vec<&str> = text.lines().collect();
        let first = all.iter().position(|line| line.contains("sbbl")).expect("the asm is written");
        let lines = &all[first - 1..first + 3];
        assert!(lines[0].contains("subl") && lines[3].contains("sbbl"), "{level}: {text}");
        let framed = lines.iter().filter(|line| line.contains("(%esp)") || line.contains("(%ebp)"));
        assert!(framed.count() >= 2, "{level}: {text}");
    }
}
