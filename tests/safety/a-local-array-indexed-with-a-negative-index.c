/* row: S2 */
/* refuse: J1 */
/* Underflow of a local, which on a downward growing stack walks into the caller's frame rather
   than out of the program. The capability of the local starts at its first byte, and an address
   below it wraps round to a long way past the end. */
int main(void) {
    int local[16];
    int i = 0;
    while (i < 4) {
        i++;
    }
    local[-i] = 7;
    return 0;
}
