/* accept: all */
/* An `asm` at file scope may write how far one of its own places is from the bytes saying so, which
   is `.long somewhere - .` and is a distance the linker works out because the two sections it
   measures between are placed by the linker. tcc's test file ends its second block with one of
   these, and every alternative instruction table in a kernel header is a run of them: a record in
   one section holding the distance to the code in another. What this reads back is the distance, by
   taking the address of the record and adding what the record holds, which has to land on the byte
   the template measured to. */

/* The block ends by going back to the text section, because what a template at file scope leaves
   the assembler standing in is where whatever follows it lands. gcc copies these directives into
   the stream it hands the assembler, so without the last line the code of `main` is assembled into
   a section that is not executable and the program built by gcc dies on its first instruction.
   rucc reads the template into globals instead and puts its own code where it always does, so it
   is right either way, and the line is here so that the file means the same thing under both. */

extern unsigned char dfh_stuff[];
extern int dfh_far[];

__asm__(".data\n"
	"dfh_stuff:\n"
	".byte 42\n"
	"661:\n"
	".byte 43\n"
	".pushsection .data.dfh, \"aw\"\n"
	".globl dfh_far\n"
	"dfh_far:\n"
	".long 661b - .\n"
	".popsection\n"
	".text\n");

int main(void)
{
	unsigned char *at = (unsigned char *)&dfh_far[0];

	if (at + dfh_far[0] != &dfh_stuff[1]) {
		return 1;
	}
	if (*(at + dfh_far[0]) != 43) {
		return 2;
	}
	return 0;
}
