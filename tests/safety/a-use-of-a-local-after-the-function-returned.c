/* row: T4 */
/* refuse: J1 */
/* The frame is gone. It cleared its witness on the way out, so the capability the returned
   pointer carries names a word that no longer says the frame is there. Not inlined, so that at
   -O2 there is still a frame to return from. */
__attribute__((noinline)) static int *escape(void) {
    int local[16];
    local[0] = 3;
    return local;
}

int main(void) {
    int *p = escape();
    return p[0];
}
