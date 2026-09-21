/* Function shapes whose line tables are worth holding against another compiler's.
 *
 * Every one of them is a shape where the two compilers have a chance of disagreeing about which
 * line the first byte of the function belongs to, which is the question the prologue answers. The
 * program is here to be compiled rather than run, so nothing in it is interesting arithmetic.
 */

static int leaf(int n) { return n + n; }

static int braced(int n)
{
    return n * 3;
}

static int spread(
    int a,
    int b
)
{
    return a - b;
}

static int locals(const int *of, int many)
{
    int sum = 0;
    int i;
    for (i = 0; i < many; i++) {
        sum += leaf(of[i]);
    }
    return sum;
}

static int branches(int n)
{
    if (n < 0) {
        return braced(n);
    } else if (n == 0) {
        return spread(n, 1);
    }
    while (n > 10) {
        n -= 7;
    }
    switch (n) {
    case 1:
        return 1;
    case 2:
        return 4;
    default:
        return n;
    }
}

int main(void)
{
    static const int some[] = { 1, 2, 3, 4 };
    int total = locals(some, 4);
    return branches(total) + leaf(1) + braced(2) + spread(3, 4);
}
