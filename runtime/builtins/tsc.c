/* The time stamp counter, for the two builtins that read it.
 *
 * gcc writes `rdtsc` and `rdtscp` in place for `__builtin_ia32_rdtsc` and `__builtin_ia32_rdtscp`.
 * rucc has no node that names an instruction from the front end, so its answer is a call to one
 * of these, which are the instruction and a return. Reading the counter takes tens of cycles on
 * the processors that have it, so the call is a small part of what the program asked for.
 *
 * Postgres 19 is what needed them. Its instr_time.h reads the counter on every x86-64 build and
 * declares nothing for the builtins, because gcc and clang know them without a header.
 *
 * Both instructions are written as their bytes, `0f 31` and `0f 01 f9`, the way programs write
 * `xgetbv`, because the assembler here has no table entry for either mnemonic yet and the bytes
 * are what any assembler would put down for them.
 *
 * The names are ours rather than anybody's library's, since no library defines a function for
 * either builtin and a name without the prefix would be one a program could have taken.
 */

#if defined(__x86_64__)

unsigned long long __rucc_ia32_rdtsc(void)
{
	unsigned int low;
	unsigned int high;
	__asm__ __volatile__(".byte 0x0f, 0x31" : "=a"(low), "=d"(high));
	return ((unsigned long long)high << 32) | low;
}

unsigned long long __rucc_ia32_rdtscp(unsigned int *aux)
{
	unsigned int low;
	unsigned int high;
	unsigned int which;
	__asm__ __volatile__(".byte 0x0f, 0x01, 0xf9" : "=a"(low), "=d"(high), "=c"(which));
	*aux = which;
	return ((unsigned long long)high << 32) | low;
}

#endif
