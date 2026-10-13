/* A C++ exception thrown in a qsort comparison goes up through the comparison, qsort and two
 * more frames of this half before the G++ half catches it. Each frame of this half needs a
 * correct .eh_frame entry, or the unwinder stops and the program aborts. */
#include <stdlib.h>

void check(int a, int b);

static int cmp(const void *x, const void *y)
{
    int a = *(const int *)x;
    int b = *(const int *)y;
    check(a, b);
    return (a > b) - (a < b);
}

__attribute__((noinline)) int smallest(int *v, int n)
{
    qsort(v, n, sizeof *v, cmp);
    return v[0];
}

__attribute__((noinline)) int twice(int *v, int n)
{
    int r = smallest(v, n);
    return r * 2;
}
