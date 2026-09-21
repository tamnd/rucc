/* accept: all */
/* `.set name, thing` in an `asm` at file scope says the name stands for the thing, and when the
   thing is another name the result is a second symbol at the first one's address. That is how a
   program gives something a default another object may override, and tcc's own test file writes
   three of them. What the directives around the `.set` said about the name is what the name
   gets: `.weak` makes it a weak symbol, `.globl` an exported one, and a name nothing said
   anything about is local, which is what an assembler does with one. A name the file goes on to
   define itself keeps its own definition. */

extern void an_override(void);
extern void a_second_name(void);
extern void defined_below(void);

void the_default(void)
{
}

__asm__(".weak an_override\n.set an_override, the_default");

/* The same name equated a second time, which is allowed and changes nothing. */
__asm__(".set an_override, the_default");

/* Exported, which is the other thing a directive says about one of these. */
__asm__(".globl a_second_name\n.set a_second_name, the_default");

/* A name the file defines below the block, where the definition is what the linker sees and the
   equate above it does not win. */
__asm__(".set defined_below, the_default");

void defined_below(void)
{
}

int main(void)
{
	an_override();
	a_second_name();
	defined_below();
	return 0;
}
