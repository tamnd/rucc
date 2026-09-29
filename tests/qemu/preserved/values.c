/* Sixteen byte values held across a call to a function that keeps only what AAPCS64 promises.
 *
 * Built by rucc at every level and by gcc as the reference, and linked against clobber.c built by
 * gcc. Every value here is live across each call to clobber, and a doubles vector and a quad
 * long double are both sixteen bytes in one vector register, so an allocator that thinks v8 to
 * v15 survive a call whole will keep them there and print the wrong numbers.
 */

#include <stdio.h>

typedef double pair __attribute__((vector_size(16)));

void clobber(void);

static volatile double seed = 1.25;

int main(void)
{
	pair a = { seed, seed * 2 };
	pair b = { seed * 3, seed * 4 };
	pair c = { seed * 5, seed * 6 };
	pair d = { seed * 7, seed * 8 };
	long double p = seed * 9;
	long double q = seed * 10;
	long double r = seed * 11;
	long double s = seed * 12;

	for (int round = 0; round < 3; round++) {
		clobber();
		a += b;
		clobber();
		b += c;
		clobber();
		c += d;
		clobber();
		d += a;
		clobber();
		p = p + q;
		clobber();
		q = q + r;
		clobber();
		r = r + s;
		clobber();
		s = s + p;
		printf("%d: %g %g %g %g %g %g %g %g\n", round, a[0], a[1], b[0], b[1], c[0], c[1],
		       d[0], d[1]);
		printf("%d: %Lg %Lg %Lg %Lg\n", round, p, q, r, s);
	}
	return 0;
}
