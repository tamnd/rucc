/* Everything around the functions under test, built by the system compiler.
 *
 * `bottom` is where the walk starts. It asks for the stack it is standing on, which goes up through
 * `inner` and `outer` and out into the program's own entry, and the number it gets back is the
 * whole of the question: an unwinder that cannot describe a frame stops at it and the count comes
 * back short. */

#include <execinfo.h>
#include <stdio.h>

void outer(void);

void bottom(void)
{
	void *frames[32];
	printf("frames: %d\n", backtrace(frames, 32));
}

int main(void)
{
	outer();
	return 0;
}
