/* The callee half of the preserved register fixture, always built by gcc.
 *
 * AAPCS64 preserves the low sixty four bits of v8 to v15 and nothing above them, so gcc saves
 * d8 to d15 around this and leaves the top halves as the asm wrote them. A caller that kept a
 * sixteen byte value in one of those registers across the call finds its top half turned to ones.
 */

void clobber(void)
{
	__asm__ volatile("movi v8.2d, #0xffffffffffffffff\n\t"
			 "movi v9.2d, #0xffffffffffffffff\n\t"
			 "movi v10.2d, #0xffffffffffffffff\n\t"
			 "movi v11.2d, #0xffffffffffffffff\n\t"
			 "movi v12.2d, #0xffffffffffffffff\n\t"
			 "movi v13.2d, #0xffffffffffffffff\n\t"
			 "movi v14.2d, #0xffffffffffffffff\n\t"
			 "movi v15.2d, #0xffffffffffffffff"
			 :
			 :
			 : "v8", "v9", "v10", "v11", "v12", "v13", "v14", "v15");
}
