/* row: S2 */
/* refuse: J1 */
/* A stack overflow that writes, which is the one that reaches the return address. No plane covers
   the stack, so it is the capability the compiler builds out of the local's own address and size
   that refuses it. */
int main(void) {
    int local[16];
    local[16] = 1;
    return local[0];
}
