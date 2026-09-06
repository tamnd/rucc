/* row: Y6 */
/* refuse: J1 */
/* gap: #431 */
/* The value is whatever the last call left on the stack, which makes the bug reproduce
   differently in a debug build and in a release one and is why it survives so long. The init
   plane is byte granular and lands at S5. */
int fill(void) {
    int noise[8];
    int i;
    for (i = 0; i < 8; i++) {
        noise[i] = 0x5a5a5a5a;
    }
    return noise[0];
}

int main(void) {
    int uninitialized[8];
    fill();
    return uninitialized[3] == 0 ? 0 : 1;
}
