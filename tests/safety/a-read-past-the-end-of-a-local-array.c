/* row: S2 */
/* refuse: J1 */
/* A stack overflow that reads. The local has no instance in any plane, and what refuses the read is
   the capability made where the array was, which knows it is sixty four bytes. */
int main(void) {
    int local[16];
    int i;
    for (i = 0; i < 16; i++) {
        local[i] = i;
    }
    return local[16];
}
