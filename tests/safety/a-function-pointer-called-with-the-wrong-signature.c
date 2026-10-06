/* row: Y4 */
/* refuse: J1 */
/* A function of one parameter called through a pointer to a function of two. The caller writes the
   count it passes beside the address it calls, and the callee compares it with its own on the way
   in, because the two ends are never in one place anywhere else. */
int one(int a) {
    return a;
}

int main(void) {
    int (*as_one)(int) = one;
    int (*as_two)(int, int) = (int (*)(int, int))as_one;
    return as_two(1, 2) == 1 ? 0 : 1;
}
