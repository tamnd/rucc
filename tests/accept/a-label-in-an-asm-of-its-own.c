/* accept: all */
/* One `asm` jumps to a local label and a later one is nothing but that label, which is how a program
   skips code the compiler cannot see is skipped. The label has to reach the assembler with its name
   on it for the jump to find it, and rucc used to read the second template into a block of its own
   and drop the name, so the file did not assemble. gcc says nothing about what the code between two
   statements does when one jumps to the other, so all this asks is that the program builds and gets
   past the label, not what `x` holds. */

int main(void)
{
	volatile int x = 1;

	__asm__ volatile("jmp 7f");
	x = 2;
	__asm__ volatile("7:");
	return x == 1 || x == 2 ? 0 : 1;
}
