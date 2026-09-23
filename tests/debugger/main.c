/* Everything around the function under test, built by the system compiler in both builds.
 *
 * `stop` does nothing at all. It is here so that there is one symbol to break on that means the
 * same program point in both builds, which a line number would not: the two compilers are free to
 * disagree about which bytes a line owns, and this check is about the locals rather than about
 * that. */

#include <stdio.h>

int examine(int count, const char *label);

void stop(void)
{
}

int main(void)
{
	printf("examine: %d\n", examine(7, "hello"));
	return 0;
}
