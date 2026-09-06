/* row: S2 */
/* refuse: J1 */
/* gap: #428 */
/* A stack overflow, which needs the plane writes at the start and end of a scope that milestone
   S2 emits. The heap is what S1 instruments. */
int main(void) {
    int local[16];
    int i;
    for (i = 0; i < 16; i++) {
        local[i] = i;
    }
    return local[16];
}
