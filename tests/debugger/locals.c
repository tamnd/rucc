/* The function whose locals are read back, built by the compiler under test.
 *
 * `stop` is where the debugger stops, and it is a call rather than a line number because a line
 * number would be comparing the two line tables as well and `cargo xtask lines` already does that.
 * The frame above the call is the one every question is asked of, and the address it is asked at
 * is a return address, so a value that is still wanted after the call is somewhere the call did
 * not clobber and a value that is not is nowhere at all.
 *
 * The two `inner`s are the point of the block half. They are declared in sibling scopes and hold
 * different values, so a reader that cannot tell them apart has no way to give the right answer
 * for `inner` and a reader that can has only one answer to give.
 *
 * `early` is the point of the other half. Nothing reads it after the line that adds it in, so at
 * the breakpoint it is a name with no value anywhere, and what a debugger should say about it is
 * that it is unavailable rather than whatever is in the register it used to be in. `count` is the
 * opposite case by design: the return needs it, so it is live across the call and has to survive
 * the call to be printed. */

void stop(void);

int examine(int count, const char *label)
{
	int total = count * 2;
	int buf[4] = {count, count + 1, count + 2, count + 3};
	int early = total + 1;
	total += early;
	{
		int inner = total + 10;
		total += inner;
		stop();
		total -= inner;
	}
	{
		int inner = total + 100;
		total += inner;
	}
	return total + buf[3] + label[0] + count;
}
