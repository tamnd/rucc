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

#if defined(__x86_64__) && defined(_WIN32) && defined(_MSC_VER)

/* Microsoft's routine, for the msvc rows, where the compiler calls `__chkstk` rather than mingw's
 * name. The contract is Microsoft's rather than mingw's: the size arrives in `rax` the same way and
 * the stack pointer is left where it was, but `r10` and `r11` are the routine's to destroy, so it
 * walks with those two and pushes nothing. A caller that keeps a value in either across the call
 * is one Microsoft's own `chkstk.obj` would break as well.
 *
 * The CRT libraries have a copy of this too, and the two answer alike, so it does not matter
 * which one a link picks. Ours is here so that a program linked with `-nostdlib`, or against a
 * CRT that is not Microsoft's, still has one. */

	.text
	.globl	__chkstk
	.type	__chkstk, @function
__chkstk:
	/* Where the frame starts, which is the stack pointer the caller had before the call. Only
	   the return address is above the stack pointer here, since nothing was pushed. */
	leaq	8(%rsp), %r10
	movq	%rax, %r11
	cmpq	$0x1000, %r11
	jb	.Lmsvc_rest
.Lmsvc_page:
	subq	$0x1000, %r10
	/* The same touch as below: an or of zero writes back what was there. */
	orq	$0x0, (%r10)
	subq	$0x1000, %r11
	cmpq	$0x1000, %r11
	ja	.Lmsvc_page
.Lmsvc_rest:
	subq	%r11, %r10
	orq	$0x0, (%r10)
	ret

#elif defined(__x86_64__) && defined(_WIN32)

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

/* Kept out of the export table of a DLL that exports every name it defines, which is a DLL with no
   dllexport and no .def file. The linker leaves libgcc's copy out because of the archive's name,
   and ours has a different name, so it says so itself. This is what the compiler writes for a
   hidden definition in C, and the rest of this directory is built hidden on Windows. */
	.section	.drectve,"yni"
	.ascii	" -exclude-symbols:___chkstk_ms"

#elif defined(__aarch64__) && defined(_WIN32)

/* The ARM64 routine, which has Microsoft's name and contract on both environments, and is what
 * compiler-rt ships for llvm-mingw. The size arrives in `x15` counted in units of sixteen bytes,
 * because the caller follows the call with `sub sp, sp, x15, lsl #4`, and `x15` has to come back
 * unchanged for that. `x16` and `x17` are the routine's to destroy, since the convention already
 * lets a veneer between caller and callee do the same, and nothing else is touched. The link
 * register is the caller's to have saved before the call, which its prologue did.
 *
 * The walk is compiler-rt's: one load a page, starting a page below the stack pointer the caller
 * has, until the pages cover the size. A load into `xzr` is enough to fault a guard page in and
 * changes no register. */

	.text
	.globl	__chkstk
	.p2align	2
__chkstk:
	lsl	x16, x15, #4
	mov	x17, sp
.Lpage_a64:
	sub	x17, x17, #0x1000
	subs	x16, x16, #0x1000
	ldr	xzr, [x17]
	b.gt	.Lpage_a64
	ret

/* Kept out of the export table of a DLL for the same reason as `___chkstk_ms` above. */
	.section	.drectve,"yni"
	.ascii	" -exclude-symbols:__chkstk"

#elif defined(__i386__) && defined(_WIN32) && defined(_MSC_VER)

/* Microsoft's i386 routine, `_chkstk` in C and so `__chkstk` here, with `_alloca_probe` as a
 * second name for it, which is what Microsoft's CRT calls the same code. Unlike every other routine
 * in this file it moves the stack pointer itself: the size arrives in `eax`, the routine comes back
 * with the stack pointer that much lower, and the caller subtracts nothing afterwards. Only `eax`
 * is the routine's to change, and it comes back holding the size as it went in. This is the walk
 * compiler-rt's `chkstk2.S` does, which reads each page rather than writing it. */

	.text
	.globl	__chkstk
	.globl	__alloca_probe
__chkstk:
__alloca_probe:
	pushl	%ecx
	cmpl	$0x1000, %eax
	/* Where the frame starts, which is the stack pointer the caller had before the call. The
	   return address and the saved `ecx` are above the stack pointer here. */
	leal	8(%esp), %ecx
	jb	.Lmsvc32_rest
.Lmsvc32_page:
	subl	$0x1000, %ecx
	testl	%ecx, (%ecx)
	subl	$0x1000, %eax
	cmpl	$0x1000, %eax
	ja	.Lmsvc32_page
.Lmsvc32_rest:
	subl	%eax, %ecx
	testl	%ecx, (%ecx)
	/* Where the return address is, and then the new stack with the return address on it. What is
	   left in `eax` once the new stack pointer is taken off is the size the caller asked for. */
	leal	4(%esp), %eax
	movl	%ecx, %esp
	movl	-4(%eax), %ecx
	pushl	(%eax)
	subl	%esp, %eax
	ret

#elif defined(__i386__) && defined(_WIN32)

/* mingw's i386 routine, `__chkstk_ms` in C and so `___chkstk_ms` here, with the contract of the
 * x86-64 one above: the size arrives in `eax`, every register comes back as it went in, and the
 * caller subtracts the size from the stack pointer afterwards. It is libgcc's walk at four bytes a
 * word. */

	.text
	.globl	___chkstk_ms
___chkstk_ms:
	pushl	%ecx
	pushl	%eax
	cmpl	$0x1000, %eax
	/* Where the frame starts. The return address and the two registers just saved are above the
	   stack pointer, which is what the twelve counts. */
	leal	12(%esp), %ecx
	jb	.Lmingw32_rest
.Lmingw32_page:
	subl	$0x1000, %ecx
	orl	$0x0, (%ecx)
	subl	$0x1000, %eax
	cmpl	$0x1000, %eax
	ja	.Lmingw32_page
.Lmingw32_rest:
	subl	%eax, %ecx
	orl	$0x0, (%ecx)
	popl	%eax
	popl	%ecx
	ret

/* Kept out of the export table of a DLL for the same reason as the x86-64 one. The linker puts
   the underscore of an i386 name back on itself, so the option names the C name, as clang's does. */
	.section	.drectve,"yni"
	.ascii	" -exclude-symbols:__chkstk_ms"

#endif
