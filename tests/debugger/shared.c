/* A function whose two arrays share their bytes, built by the compiler under test at `-O2`.
 *
 * `spent` is finished with before `later` is first written, so from `-O1` up the frame puts the two
 * in the same bytes. At the breakpoint `spent` is dead and its bytes hold `later`, which is the
 * shape where a place given for the whole function would have a debugger print one array under the
 * other's name. So what is asked is that `later` reads back the same as gcc's and that `spent` is
 * said to be unavailable rather than read out of bytes that are no longer its own.
 *
 * The indexes are worked out from the argument so that both arrays stay in memory. With constant
 * ones an optimizer is free to keep the elements in registers, and then nothing shares at all and
 * the question is not asked. Only the elements written are printed, since the rest hold whatever
 * was on the stack and the two builds have no reason to agree about that. */

void stop(void);

int reuse(int n)
{
	int spent[16];
	spent[n & 15] = n;
	spent[(n + 1) & 15] = n + 7;
	int sum = spent[n & 15] + spent[(n + 1) & 15];
	int later[16];
	later[sum & 15] = sum;
	later[(sum + 3) & 15] = sum * 2;
	stop();
	return later[sum & 15] + later[(sum + 3) & 15];
}
