//! The four texts the printer, the parser and the verifier are all checked against.
//!
//! One copy, three claims each. The printer writes exactly this, the parser reads exactly this back,
//! and the verifier says all of it is a module the rest of the compiler may believe. Keeping the
//! three in one place is what stops a change to the printer from being blessed into a fixture
//! nobody read: a person has to look at the diff here, and the other two files then have to
//! agree with what that person approved.

/// The example from the spec, which is what `print` produces for it.
pub(crate) const EXAMPLE: &str = "\
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
";

/// One function holding very nearly every opcode, which is what `print` produces for it.
pub(crate) const ZOO: &str = "\
; ModuleID = 'zoo.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"

func @zoo(i32, ptr) -> i32, linkage(external) {
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
    %15 = call @puts(%1, %5 byval(16, align 8)) : (ptr, ...) -> i32
    %16 = call_indirect %1(%0) : (i32) -> i32
    memcpy %5, %1, size 16, align 8
    inline_asm.volatile \"pause\", \"\", \"memory\"()
    %17 = va_object %1, size 16, align 8, in(int 8 at 0, float f64 at 8)
    %18 = target_intrinsic.i32 @x86.sse2.pmovmskb(%4)
    jump block1

block1:
    switch %0, block2, [0 => block3(%0), -1 => block2]

block2:
    %19 = block_addr block4
    indirect_br %19, block4

block3(%20: i32):
    return %20

block4:
    inline_asm \"jmp %l0\", \"\", \"\"(), labels [block3(%0)]
}

!0 = tbaa \"int\", offset 0
";

/// Every shape a symbol comes in, which is what `print` produces for them.
pub(crate) const SYMBOLS: &str = "\
; ModuleID = 'data.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"

global @table : bytes 28 = { bytes \"hi\\00\\ff\\\"\\\\\", zero 2, i32 7, addr.8 @hi.str + 8, addr.8 @hi.str - 8 }, align 8, linkage(external), constant, section \".rodata.rel\"
global @errno : bytes 4, align 4, linkage(external), visibility(hidden), tls(initial_exec)
global @nothing : bytes 0 = {}, align 1, linkage(internal)

alias @total = @table, linkage(weak)
ifunc @memcpy = @memcpy.resolve, linkage(external), visibility(protected)

func @puts(ptr) -> i32, linkage(external), attrs(nounwind, willreturn);

func @helper() -> (i32, i32), linkage(internal), attrs(always_inline, readnone), section \".text.hot\" {
block0:
    %0 = iconst.i32 1
    return %0, %0
}
";

/// The memory safety instructions, which is what `print` produces for them.
///
/// They are apart from [`ZOO`] because nothing emits one unless `-fsafety` asked for it, so a
/// function holding both would not be a function the compiler ever builds. The exit criterion of
/// milestone S0 in `spec/safe-memory/16-milestones.md` is that every one of them round trips
/// through the text unchanged, and this is where that is claimed.
pub(crate) const SAFETY: &str = "\
; ModuleID = 'safety.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"

func @safety(ptr, i64) -> ptr, linkage(external) {
block0(%0: ptr, %1: i64):
    %2 = cap_of %0
    %3 = cap_null
    %4 = cap_recover %0
    %5 = cap_load %0
    %6 = iconst.i64 8
    %7 = cap_narrow %2, %1, %6
    cap_store %0, %7
    %8 = ptr_add %0, %1
    check_bounds %2, %0, size 4, align 4
    check_live %2, %0
    check_type %2, %0, size 4, align 4, tbaa !0
    check_init %2, %0, size 4, align 1
    check_deriv %2, %0, %8
    check_race %2, %0
    meta_begin %0, %1, class allocated
    meta_type %0, %1, tbaa !0
    meta_init %0, %1
    meta_transfer %0, %1, to device
    meta_end %0, %1
    return %0
}

!0 = tbaa \"int\", offset 0
";
