/* row: T4 */
/* refuse: J1 */
/* The frame is gone and the next call will reuse it, so the write would land in whatever that
   call puts there. The witness the frame cleared on the way out is what says so first. Not
   inlined, so that at -O2 there is still a frame to return from. */
__attribute__((noinline)) int *escape(void) {
    int local = 7;
    return &local;
}

int main(void) {
    int *p = escape();
    *p = 1;
    return 0;
}
