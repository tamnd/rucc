/* The stack probe a Windows prologue calls, which is the one routine here that cannot be C.
 *
 * Design: `spec/12-abi-and-runtime.md` section 12.8, which says what ships is compiled by this
 * compiler, and `rucc_target::Chkstk`, which says why the platform has this at all. A thread on
 * Windows is given a stack of which only a little is committed and one page below that is a guard
 * page whose whole job is to be touched. A frame larger than a page has to be reached a page at a
 * time or the guard is stepped over and the program takes an access violation on an address that
 * was never going to be mapped.
 *
 * Assembly rather than C because the calling convention is not one C can state. The size arrives in
 * `rax`, which is not a register any argument arrives in, and the routine has to come back with
 * every register it was entered with still holding what it held, `rax` included, because the caller
 * subtracts it from the stack pointer afterwards. A C function compiled for this target would
 * destroy `rcx`, `rdx`, `r8` through `r11` and `rax` on its way through, and the callers are not
 * only ours: mingw-w64's own `libmingw32.a` and `libmingwex.a` call this, which is how every
 * program that calls `printf` reaches it. tamnd/rucc#1514.
 *
 * This is the file that makes `runtime/builtins` more than C, and the answer to where target
 * assembly lives is here beside the C rather than in a directory of its own. `cargo xtask builtins`
 * hands the whole directory to one compiler invocation and the compiler reads a `.S` the way it
 * reads a `.c`, through the preprocessor and then through its own assembler, so a file that is not
 * for this target preprocesses away to nothing and an architecture that grows a routine of its own
 * adds an arm below rather than a build rule.
 */

#if defined(__x86_64__) && defined(_WIN32)

	.text
	.globl	___chkstk_ms
	.type	___chkstk_ms, @function
___chkstk_ms:
	/* Both of the registers this walks with, because the caller is promised neither moves.
	   The return address is already on the stack above them, which is what the 24 below counts. */
	pushq	%rcx
	pushq	%rax
	cmpq	$0x1000, %rax
	/* Where the frame starts, which is the stack pointer the caller had before the call. */
	leaq	24(%rsp), %rcx
	jb	.Lrest
.Lpage:
	subq	$0x1000, %rcx
	/* Touching it is the whole point. An or of zero writes back what was already there, so the
	   page is faulted in and committed without the bytes in it changing. */
	orq	$0x0, (%rcx)
	subq	$0x1000, %rax
	cmpq	$0x1000, %rax
	ja	.Lpage
.Lrest:
	/* What is left over, which is between one byte and one whole page. */
	subq	%rax, %rcx
	orq	$0x0, (%rcx)
	popq	%rax
	popq	%rcx
	ret

#endif
