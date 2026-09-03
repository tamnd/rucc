# Assembly, objects, and debug information

## 11.1 The integrated assembler

We encode instructions directly to bytes rather than emitting text and calling `as`. Three reasons: an external assembler is a process spawn and a re-parse per translation unit, which is a large fraction of the compile-throughput budget; it is a dependency on GNU binutils or the Apple toolchain, which contradicts the portability axis; and the assembler is the place where a compiler's own instruction encoding gets validated, so having it in-tree means the validation is in-tree.

`-S` still emits assembly text, because people read it and because the assembly output is a debugging artifact we care about. The text path and the binary path share the same instruction description, so they cannot disagree about what an instruction is.

That shared description lives beside the register file, in `rucc-target`. For each machine opcode it holds the list of real instructions the opcode is, since an opcode is one instruction to the register allocator and is not always one instruction to the machine: a comparison is a compare and a set, an unsigned division is the clearing of the high half and then the division, and the pseudos that say where a value is are no instructions at all. Each instruction in the list is a mnemonic and a list of arguments, and an argument names the operand it is drawn from by its index rather than being the operand itself, because the assembler's order is not the operand vector's order, an instruction may write one operand twice, and an instruction in the middle of an opcode may name none of them. The width an argument is written at is on the argument, since a comparison of two 64-bit registers sets a byte and a shift of one reads its count from a byte.

An opcode nothing selects is described the same way as one a rule selects. A prologue pushes, a spill stores and a copy moves, and none of those is anything a pattern could match, but all of them reach the assembler and the encoder in the same function and are read there the same way. A second table for them would be a second place for an opcode to be missing from.

**We also accept assembly as input.** `.s` and `.S` files appear in every real project. The kernel has thousands, and musl, FFmpeg and OpenSSL all ship hand-written assembly. A compiler that cannot assemble them needs an external assembler after all, which forfeits the point. So `rucc-asm` contains a real assembler with a real directive set: `.text .data .bss .section .globl .local .weak .type .size .align .balign .p2align .byte .short .long .quad .ascii .asciz .space .fill .set .equ .org .comm .lcomm`, the CFI directives `.cfi_startproc` through `.cfi_endproc` with the full register-rule set, `.macro`/`.endm`, `.rept`, `.if`/`.else`/`.endif`, and expression evaluation with the symbol arithmetic and relocation-generating operators (`sym@GOT`, `sym@PLT`, `sym@TPOFF` and the rest).

Both the AT&T and Intel syntaxes are accepted for x86, selected by `-masm=` and by `.intel_syntax`/`.att_syntax` directives, because inline assembly in the wild uses both.

**Encoding correctness is verified by differential disassembly.** Every instruction we encode is disassembled by an independent decoder and compared against the intended semantics; additionally, a CI job assembles the corpus's `.s` files with both `rucc` and the system assembler and compares the resulting bytes. Encoding bugs are silent and catastrophic, and this is the cheapest way to find them.

Relaxation, choosing the short form of a jump when the displacement fits, and growing it when it does not, is an iterate-to-fixpoint pass over the fragment list. It must terminate, and it must be deterministic; both are tested.

## 11.2 Inline assembly

GCC's inline assembly is one of the least-specified and most-depended-upon parts of the C ecosystem, and it is a hard requirement for the kernel.

The pieces: a template string with `%0`-style operand references and the `%` modifier letters that select an operand's spelling (`%w0` for the 32-bit form of an AArch64 register, `%b0` for a byte register on x86, and so on); output operands with `=` and `+` and `&` earlyclobber; input operands; constraint strings; a clobber list including `"memory"` and `"cc"`; the `volatile`, `inline` and `goto` qualifiers; and for `asm goto`, a label list making the statement a terminator.

**Constraints are a per-target language** and they must be implemented per target rather than approximated. The common ones (`r` `m` `i` `n` `g` `0`-`9` for matching operands) plus each target's specific set: on x86 `a b c d S D A q Q x y v` and the immediate ranges `I` through `P`; on AArch64 `w x r Q S Y Z` and the logical-immediate classes; on RISC-V `f A I J K`. The constraint determines which register class the allocator must use and whether a memory operand is acceptable, so it feeds directly into document 10's operand constraints.

The `"memory"` clobber is a full compiler barrier for memory: no load or store may be moved across it. The kernel's `barrier()` is exactly this and getting it wrong produces bugs that appear only under concurrency and only sometimes.

**`asm goto` with outputs** is required by current kernels and is the hardest case, because the outputs must be live on the fall-through edge and not on the indirect edges, which interacts with register allocation in a way that needs explicit handling rather than falling out.

Because we cannot verify the contents of an assembly template, inline assembly is a hard barrier for the optimizer: an `inline_asm` instruction with a `"memory"` clobber or the `volatile` qualifier is treated as an unknown call for alias analysis and is never moved, duplicated, or deleted unless its result is unused and it is neither volatile nor clobbering.

Top-level `asm` at file scope is emitted verbatim into the output in source order, which the kernel uses for building alternative-instruction tables and static call sites.

## 11.3 Object files

Three formats, all written through the [`object`](https://crates.io/crates/object) crate's writer with our own layer above it for the parts it does not model.

**ELF** for Linux and the freestanding targets. Sections, symbols with the full binding and visibility matrix, `SHF_*` flags, section groups for COMDAT, `.init_array`/`.fini_array` for constructors with priorities, `.note.GNU-stack` (whose absence makes the stack executable, a real and recurring security bug), `.note.gnu.property` for CET and BTI markers, and `-ffunction-sections`/`-fdata-sections` for linker garbage collection. Relocations per target: the x86-64 set including the TLS relaxation-eligible ones, the AArch64 set including the ADRP/ADD pairs and their TLS forms, and the RISC-V set including the `R_RISCV_RELAX` markers and the paired HI20/LO12 relocations, which are their own small nightmare because the LO12 relocation references the HI20's *symbol* through a label.

**Mach-O** for Apple platforms. Segments and sections, the `__TEXT`/`__DATA`/`__DATA_CONST`/`__LD` layout, symbol table plus indirect symbol table, scattered and paired relocations, `.subsections_via_symbols` for dead stripping, and the platform version load command that current linkers require.

**COFF** for Windows. Sections, symbols with storage classes, COMDAT selection kinds, `.pdata` and `.xdata` for SEH unwind, and the associative-section mechanism that makes `-ffunction-sections`-equivalent behavior work.

The layer above `object` handles what it does not: relocation *selection* per target and per addressing mode, symbol attribute mapping from our IR's linkage and visibility, section naming conventions per platform, and the alignment and ordering rules that each linker expects but no format document states.

## 11.4 DWARF

DWARF 5 by default, DWARF 4 under `-gdwarf-4`, written through [`gimli`](https://crates.io/crates/gimli)'s write support.

At `-O0` the obligation is complete: every function has a `DW_TAG_subprogram` with correct low and high PC and frame base; every local variable has a location valid over its whole scope; every type is described, including the C-specific cases that are easy to get wrong: bit-fields with their offsets and widths, anonymous struct and union members, flexible array members, variably-modified array types whose bounds are runtime values referencing a location expression, and function pointer types with their parameter types.

At `-O2` the obligation is best-effort and honest. A variable that lives in different registers at different points gets a location list. A variable that is dead at a point is marked as such rather than given a wrong location, because a debugger showing a stale value is worse than one saying "optimized out". Inlined functions get `DW_TAG_inlined_subroutine` with the call site's location, so a backtrace through inlined frames is correct. This is the feature that makes `-O2 -g` usable at all and it requires the inlining chain from document 08 to survive every subsequent pass.

Line tables are generated from the source locations in the IR. The rules that matter for debugger behavior: `is_stmt` marks the instruction a breakpoint should attach to; the prologue-end marker tells the debugger where to stop for a function breakpoint so that arguments are already in place; a line entry with line 0 marks compiler-generated code with no source correspondence, which is better than attributing it to an arbitrary line.

`-gsplit-dwarf` writes the bulk into a `.dwo` file and leaves a skeleton in the object, which on a large C++ project is transformative and on a large C project still meaningful. `-gz` compresses debug sections. `-fdebug-prefix-map` and `-ffile-prefix-map` rewrite paths for reproducible builds, and `SOURCE_DATE_EPOCH` is honored per document 03.

**Correctness testing** is by differential debugging: compile a corpus program with `-O0 -g` and with `-O2 -g`, run it under a scripted GDB and LLDB session that sets breakpoints and prints variables at defined points, and compare against the same script run against GCC's output. This finds the errors that a DWARF *validator* cannot, because syntactically valid DWARF can still describe the wrong thing.

## 11.5 Other metadata formats

**CTF and BTF.** The kernel generates BTF for BPF and for its own runtime type information, currently via `pahole` reading DWARF. Emitting BTF directly is not a 1.0 requirement (the kernel build's existing `pahole` step works on our DWARF if the DWARF is correct) but "our DWARF is correct enough for `pahole`" is an M11 exit criterion in document 14 and it is a strictly stronger test than a validator.

**Sanitizer metadata.** ASan needs global variable descriptors with redzones and their metadata section; UBSan needs static check descriptors with source locations. Document 12 owns their layout, which must match the runtime library's expectations exactly.

## 11.6 Linking

We invoke an external linker before 1.0, per document 04. The interesting question is whether that stays true.

**The case for an internal linker after 1.0.** The kernel's build uses linker scripts of real complexity, `--emit-relocs`, section garbage collection interacting with `KEEP`, and a two-pass link with `objtool` in between. Every one of those is a place where the external linker's behavior differs between `ld.bfd`, `lld` and `mold`, and where diagnosing a problem means understanding three linkers. Owning the linker would also close the portability story: one binary that compiles and links on all three hosts without a platform toolchain installed. And link time is a large share of a full-project build, so the throughput axis is partly gated on someone else's code.

**The case against.** A linker is a substantial project on its own; `mold` is already very fast and free; and the failure mode of a subtly wrong linker is worse than that of a subtly wrong compiler because it is harder to bisect.

The decision is deferred to document 19 and gated on a measurement: at M11, what fraction of a kernel build's wall time is linking, and how many of the bugs found during M10 and M11 were linker interaction bugs. If both numbers are high the project is justified; otherwise `mold` is the answer.

Until then the driver's obligation is to construct a correct link line per target and to pass through everything the build system asks for, which document 04 specifies and which the corpus in document 15 tests far more thoroughly than any unit test could.
