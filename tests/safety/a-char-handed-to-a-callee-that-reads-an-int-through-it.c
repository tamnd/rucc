/* row: S2 */
/* refuse: J1 */
/* A local `char` handed to a callee that reads four bytes through the pointer. The caller does
   nothing with the address itself, so once its own checks are gone there is nothing left in it
   that wants the capability, and the callee used to get a cleared frame and fall back to the
   planes, which know nothing about the stack. The read went through at -O1 and up. The caller now
   makes the capability for a local at the call when it has none, and the callee is handed it. It
   is noinline so the read stays on the other side of a call. */
__attribute__((noinline)) int read_an_int(void *raw) {
    return *(int *)raw;
}

int main(void) {
    char c = 97;
    return read_an_int(&c);
}
