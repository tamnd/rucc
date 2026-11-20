//! What `__attribute__((ms_hook_prologue))` lays down around a function, which is gcc's x86 layout
//! of a function a Windows hot patcher can redirect while the program runs.
//!
//! Two runs of bytes. The one after the label is an instruction that does nothing, or on i386
//! three that build the frame pointer the way Microsoft's compiler does, which a patcher replaces
//! with a two byte jump back into the other run. That one is in front of the label, filled with
//! `int3`, and is where the patcher writes a jump long enough to reach its replacement. Both are
//! written as bytes rather than as instructions, because the patcher checks for these bytes
//! before it writes anything, and an encoder picks its own forms: `mov %edi, %edi` is `89 ff` to
//! gas and `8b ff` to the patcher.
//!
//! On i386 the three leave the caller's frame pointer pushed and the stack pointer in it, which is
//! what gcc's frame there is built on when it keeps a frame pointer. The prologue here takes it
//! off again and builds its own, which is what gcc does when it keeps none, and the two run the
//! same. See `rucc_codegen::finish`.

use rucc_tuple::Arch;

/// The bytes after the label: `lea 0(%rsp), %rsp` with a four byte displacement on x86-64, and
/// `mov %edi, %edi`, `push %ebp` and `mov %esp, %ebp` on i386. Nothing on any other machine.
pub(crate) fn opening(arch: Arch) -> &'static [u8] {
    match arch {
        Arch::X86_64 => &[0x48, 0x8d, 0xa4, 0x24, 0x00, 0x00, 0x00, 0x00],
        Arch::X86 => &[0x8b, 0xff, 0x55, 0x8b, 0xec],
        _ => &[],
    }
}

/// How many `int3` bytes go in front of the label, which is room for a jump anywhere on x86-64 and
/// for a five byte one on i386, rounded up as gcc rounds it.
pub(crate) fn room(arch: Arch) -> usize {
    match arch {
        Arch::X86_64 => 32,
        Arch::X86 => 16,
        _ => 0,
    }
}

/// The byte the room is filled with, which stops a processor that wandered into it.
pub(crate) const INT3: u8 = 0xcc;

/// The bytes as the directive gcc writes them in, one line.
pub(crate) fn spelled(bytes: &[u8]) -> String {
    let list: Vec<String> = bytes.iter().map(|byte| format!("{byte:#04x}")).collect();
    format!("\t.byte\t{}\n", list.join(", "))
}
