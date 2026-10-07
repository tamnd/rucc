/* row: Y6 */
/* refuse: J1 */
/* The value is whatever the last call left on the stack, which makes the bug reproduce
   differently in a debug build and in a release one and is why it survives so long. The stack
   plane is told the array begins unwritten, the stores in fill() land somewhere else, and the
   read is refused. */
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
