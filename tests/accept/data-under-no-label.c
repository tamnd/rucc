/* accept: all */
/* An `asm` at file scope may write bytes before it writes any label, name a label with a number,
   and move to another section and come back. tcc's test file writes all three in one block, which
   is what this is: a byte under no label, the name the program reads through, a local label, the
   byte it is measuring, a second local label, a push to a section nothing reads, and the distance
   between the two local labels written as a byte. The bytes under no label still have to land in
   front of the label written after them, because the program reads backwards from the name and
   gets the byte above it, and the local labels have to leave the bytes either side of them in one
   piece, because the distance between them is what the last line writes. */

extern unsigned char alld_stuff[];

__asm__(".data\n"
	".byte 41\n"
	"alld_stuff:\n"
	"661:\n"
	".byte 42\n"
	"662:\n"
	".pushsection .data.ignore\n"
	".byte 7\n"
	".popsection\n"
	".byte 662b - 661b\n");

int main(void)
{
	if (alld_stuff[0] != 42) {
		return 1;
	}
	if (alld_stuff[1] != 1) {
		return 2;
	}
	if (alld_stuff[-1] != 41) {
		return 3;
	}
	return 0;
}
