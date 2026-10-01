//! GNU's nested functions, and what the functions around one have to do for it.
//!
//! Design: `spec/13-gnu-compat.md` section 13.3.
//!
//! A nested function is an ordinary function with no linkage, under a numbered name the way a
//! `static` in a function is, that takes one parameter more than it says: the static chain, which
//! is how it reaches the variables of the functions it is written inside. The chain is the address
//! of an environment block in the frame of the function the nested one is defined in. The block
//! is one word for the trampoline, which is filled only when the function's address is taken,
//! and then one word for each thing the nested function reaches that is not its own, each holding
//! the address of that thing as the defining function sees it. A variable's entry is the
//! variable's address, which is why everything a nested function reaches lives in memory. A
//! nested function's entry is the address of that function's own block, which is its chain, so a
//! call from one nested function to a sibling, or to a nested function further out, knows what to
//! pass.
//!
//! Everything a nested function reaches through its chain is listed here, in [`Frame::captures`],
//! and the list is worked out once for the whole tree of functions under one ordinary function,
//! from the inside out. What a nested function reaches includes what the functions inside it
//! reach and is not its own, since those reach it through the block this one builds for them.
//!
//! A direct call passes the block as the chain, in the register [`rucc_target::CallRegs::chain`]
//! names. A nested function whose address is taken is called by whoever has the address with no
//! chain at all, which is what the trampoline is for: libgcc's
//! `__gcc_nested_func_ptr_created` makes one on the heap that loads the chain and jumps to the
//! function, and writes its address in the first word of the block, and the defining function
//! gives it back with `__gcc_nested_func_ptr_deleted` on its way out. That is what gcc 14 and later
//! do under `-ftrampoline-impl=heap`, and it is the only way here, because the other way is code
//! written on the stack and a stack that can be executed is something no current system allows.
//!
//! gcc's trampoline loads the chain into `r10` on x86-64, and this compiler keeps `r10` back from
//! the allocator for the code written after allocation, so the chain travels in `rax` instead and
//! the trampoline is pointed at a stub of two instructions in front of the function that moves it
//! across. On AArch64 the chain is `x18` on both sides and the trampoline goes straight to the
//! function.

use rucc_base::hash::{Map, Set};
use rucc_sema::DeclId;

/// What the walk knows about the nested functions of the file so far.
#[derive(Debug, Default)]
pub(crate) struct Nest {
    /// Each function in a tree that has a nested function in it, the ordinary one at the top
    /// included.
    pub(crate) frames: Map<DeclId, Frame>,
    /// Every automatic object some nested function reaches, which is what has to be in memory in
    /// the function that declares it, since its address is what the block holds.
    pub(crate) captured: Set<DeclId>,
    /// Whether the target has already been said to have no nested functions, which is said once.
    pub(crate) refused: bool,
}

/// One function of a tree of nested ones.
#[derive(Debug, Default, Clone)]
pub(crate) struct Frame {
    /// Whether this is a nested function, and so one with a chain, rather than the ordinary
    /// function at the top of the tree.
    pub(crate) nested: bool,
    /// What the function reaches through its chain, in the order the words of its block hold
    /// them, after the first one.
    pub(crate) captures: Vec<DeclId>,
    /// Whether the function's address is taken anywhere, which is what needs a trampoline.
    pub(crate) escapes: bool,
    /// The nested functions defined directly in this one, each with a block in this frame.
    pub(crate) children: Vec<DeclId>,
}

impl Nest {
    /// Whether `decl` is a nested function, which is a function with a chain.
    pub(crate) fn is_nested(&self, decl: DeclId) -> bool {
        self.frames.get(&decl).is_some_and(|frame| frame.nested)
    }

    /// What is known about `decl` as one function of a tree, if it is one.
    pub(crate) fn frame(&self, decl: DeclId) -> Option<&Frame> {
        self.frames.get(&decl)
    }
}

/// The symbol the trampoline of a nested function is pointed at on a machine whose trampolines
/// leave the chain somewhere the function does not read it, which is x86-64. See the module
/// documentation for why there is one.
pub(crate) fn stub_name(function: &str) -> String {
    format!("{function}.chain")
}

/// The stub itself, as a block of assembly at file scope: the chain from where libgcc's
/// trampoline left it to where the function reads it, and then the function.
pub(crate) fn stub(function: &str) -> String {
    let stub = stub_name(function);
    format!(
        "\t.text\n\t.p2align 4\n\t.type {stub}, @function\n{stub}:\n\tmovq %r10, %rax\n\tjmp \
         {function}\n\t.size {stub}, .-{stub}\n"
    )
}
