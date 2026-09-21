/* accept: all */
/* An `asm` at file scope may write bytes before it writes any label, name a label with a number,
   and move to another section and come back. tcc's test file writes all three in one block, which
   is what this is: a byte under no label, the name the program reads through, a local label, the
   byte it is measuring, a second local label, a push to a section nothing reads, and the distance
   between the two local labels written as a byte. The bytes under no label still have to land in
   front of the label written after them, because the program reads backwards from the name and
   gets the byte above it, and the local labels have to leave the bytes either side of them in one
   piece, because the distance between them is what the last line writes. */

/* The block ends by going back to the text section, because what a template at file scope leaves
   the assembler standing in is where whatever follows it lands. gcc copies these directives into
   the stream it hands the assembler, so without the last line the code of `main` is assembled into
   a section that is not executable and the program built by gcc dies on its first instruction.
   rucc reads the template into globals instead and puts its own code where it always does, so it
   is right either way, and the line is here so that the file means the same thing under both. */

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
	".byte 662b - 661b\n"
	".text\n");

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
