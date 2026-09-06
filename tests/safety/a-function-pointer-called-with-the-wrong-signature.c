/* row: Y4 */
/* refuse: J1 */
/* gap: #431 */
/* The callee reads a second argument the caller never passed, so it gets whatever was in the
   register. Catching this needs the signature recorded beside the function's address, which is
   the type plane's job and lands at S5. */
int one(int a) {
    return a;
}

int main(void) {
    int (*as_one)(int) = one;
    int (*as_two)(int, int) = (int (*)(int, int))as_one;
    return as_two(1, 2) == 1 ? 0 : 1;
}
