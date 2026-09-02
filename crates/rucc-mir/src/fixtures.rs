//! The register file and the two texts the printer and the parser are both checked against.
//!
//! One copy, two claims. The printer writes exactly this, and the parser reads exactly this
//! back. Keeping the texts in one place is what stops a change to the printer from being
//! blessed into a fixture nobody read: a person has to look at the diff here, and the other two
//! files then have to agree with what that person approved.
//!
//! The register file is a fixture too. It is shaped like x86-64's because reading a dump full
//! of made-up register names is harder than it needs to be, and it is not x86-64's, because the
//! real one belongs to the target that uses it and arrives with it.

use rucc_target::{ClassInfo, RegFile};

static GPR: [&str; 8] = ["rax", "rcx", "rdx", "rbx", "rsi", "rdi", "rsp", "rbp"];
static XMM: [&str; 4] = ["xmm0", "xmm1", "xmm2", "xmm3"];

static CLASSES: [ClassInfo; 2] = [
    ClassInfo { name: "gpr", bits: 64, regs: &GPR },
    ClassInfo { name: "xmm", bits: 128, regs: &XMM },
];

/// The registers the fixtures are written in.
pub(crate) static REGS: RegFile = RegFile::new(&CLASSES);

/// A function as it is before allocation, which is what `--emit=mir` writes.
pub(crate) const BEFORE: &str = "\
mfunc @scale {
block0(%0:gpr, %1:gpr):
    %2:gpr = x64.mov_ri 4
    %3:gpr(reuse 1) = x64.imul_rr %1, %2
    x64.cmp_ri %0, 0
    x64.jle block2(%0), block1(%3, %1)

block1(%4:gpr, %5:gpr):
    %6:gpr = x64.lea [%4 + %5*4 + 16]
    %7:gpr = x64.mov_rm [@counter + 8]
    x64.mov_mi [%6 - 4], 1
    %8:gpr($rax), early %9:gpr($rdx) = x64.idiv_rr %7($rax), %6(any)
    x64.cmp_rr %8, %9(stack)
    x64.jmp block2(%8)

block2(%10:gpr):
    %11:xmm = x64.movd_xr %10
    x64.ret $rax
}
";

/// The same function after allocation, which is what `--emit=mir-final` writes.
///
/// No virtual registers and no block parameters: the parameters have become physical registers
/// and the moves that were the arguments on the edges have been written out, which is the point
/// at which MIR stops being in SSA form.
pub(crate) const AFTER: &str = "\
mfunc @scale {
block0:
    $rcx = x64.mov_ri 4
    $rax = x64.imul_rr $rax, $rcx
    x64.cmp_ri $rdi, 0
    x64.jle block2, block1

block1:
    $rdx = x64.lea [$rax + $rcx*4 + 16]
    x64.mov_mi [$rdx - 4], 1
    x64.jmp block2

block2:
    x64.ret $rax
}
";
