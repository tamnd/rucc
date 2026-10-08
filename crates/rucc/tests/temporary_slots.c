/* Calls that return a structure and statement expressions with a local whose address is handed
 * out, one statement each. The slot behind each of them is wanted until the end of its statement
 * and no further, so the four in each function can be the same bytes. */

struct big {
	long words[36];
};

extern struct big make(long seed);
extern void fill(struct big *out, long seed);
extern long sum(const long *words);

/* Four calls that return a structure, each read in a statement of its own. */
long calls(void)
{
	long total = sum(make(1).words);
	total += sum(make(2).words);
	total += sum(make(3).words);
	total += sum(make(4).words);
	return total;
}

/* A macro over a statement expression, the way the intrinsic headers and the kernel write them. */
#define FILLED(seed) ({ struct big b_; fill(&b_, (seed)); sum(b_.words); })

long macros(void)
{
	long total = FILLED(1);
	total += FILLED(2);
	total += FILLED(3);
	total += FILLED(4);
	return total;
}

/* A statement expression whose value is its own local, which is read after the braces close. */
#define KEPT(seed) ({ struct big k_; fill(&k_, (seed)); k_; })

long kept(void)
{
	struct big one = KEPT(5);
	long total = sum(one.words) + sum(KEPT(6).words);
	struct big two = KEPT(7);
	return total + sum(one.words) + sum(two.words);
}

#ifdef RUN
#include <stdio.h>

struct big make(long seed)
{
	struct big b;
	for (int i = 0; i < 36; i++)
		b.words[i] = seed * 100 + i;
	return b;
}

void fill(struct big *out, long seed)
{
	*out = make(seed);
}

long sum(const long *words)
{
	long s = 0;
	for (int i = 0; i < 36; i++)
		s += words[i];
	return s;
}

int main(void)
{
	printf("%ld %ld %ld\n", calls(), macros(), kept());
	return 0;
}
#endif
