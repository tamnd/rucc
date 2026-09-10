/* The functions this compiler writes, which are the ones the unwinder has to walk through.
 *
 * Two of them, with different frames on purpose. `inner` takes more arguments than there are
 * argument registers and does enough with them to need callee saved registers and a frame of its
 * own, so its record has a saved register in it as well as a moved stack pointer. `outer` takes a
 * small frame and nothing else, which is the shape most functions in a program are.
 *
 * Neither may be inlined. The count this is compared against is the system compiler's for the same
 * source, and a compiler that folded the two into one would be counting a different stack. */

void bottom(void);

static long total;

__attribute__((noinline)) void inner(long a, long b, long c, long d, long e, long f, long g)
{
	long s = 0;
	for (long i = 0; i < g; i++)
		s += a * b + c * d + e * f + i;
	total += s;
	bottom();
}

__attribute__((noinline)) void outer(void)
{
	inner(1, 2, 3, 4, 5, 6, 7);
}
