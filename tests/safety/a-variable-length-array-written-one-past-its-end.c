/* row: S2 */
/* refuse: J1 */
/* The length is an argument, so the array is an `alloca` with its size as an operand rather than
   a number, and that used to be left to recovery, which says everything for a stack address. The
   capability is made out of the operand now, so the last store, one element past the end, is
   refused rather than landing on whatever the frame keeps next to it. */
int fill(int count) {
    int values[count];
    int i;
    for (i = 0; i <= count; i++) {
        values[i] = i;
    }
    return values[count - 1];
}

int main(void) {
    return fill(16) == 15 ? 0 : 1;
}
