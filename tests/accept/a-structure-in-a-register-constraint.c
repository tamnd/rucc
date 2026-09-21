/* accept: all */
/* A structure has no value of its own that a register holds, and a register constraint over one
   as wide as a register still means something: the bytes of the object are the number the
   assembly is handed, and an output of that kind is written back into the object. gcc does this
   and real code depends on it, tcc's own test file among it, where a one word structure is put
   through `asm volatile("" : "=r" (ret) : "0" (s))` to check that the compiler can load a
   structure of the right shape into a register at all. */

struct word {
	unsigned long addr;
};

struct two_shorts {
	unsigned short a, b;
};

struct one_byte {
	unsigned char a;
};

/* An input, which is read out of wherever the object is. */
static unsigned long read_it(struct word w)
{
	unsigned long r;
	__asm__("" : "=r"(r) : "0"(w));
	return r;
}

/* An output, which is written back into the object. */
static struct word write_it(void)
{
	struct word w;
	__asm__("" : "=r"(w));
	return w;
}

/* Both at once, which is one operand and not two. */
static struct word both_ways(struct word w)
{
	__asm__("" : "+r"(w));
	return w;
}

/* The other widths a register has, since the rule is the size of the object and not what is in
   it. A structure still in memory is unaffected, which is the case that always worked. */
static unsigned int narrower(struct two_shorts t, struct one_byte b)
{
	unsigned int r;
	unsigned char c;
	__asm__("" : "=r"(r) : "0"(t));
	__asm__("" : "=r"(c) : "0"(b));
	return r + c;
}

static void in_memory(struct word w)
{
	__asm__("" : : "m"(w));
}

int main(void)
{
	struct word w;
	struct two_shorts t;
	struct one_byte b;
	w.addr = 1;
	t.a = 0;
	t.b = 0;
	b.a = 0;
	in_memory(w);
	return (int)(read_it(w) - 1) + (int)write_it().addr * 0 + (int)both_ways(w).addr - 1
	       + (int)narrower(t, b);
}
