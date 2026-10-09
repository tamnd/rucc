/* row: S2 */
/* refuse: J2 */
/* Underflow of a local, which on a downward growing stack walks into the caller's frame rather
   than out of the program. The capability of the local starts at its first byte, so `&local[-i]`
   has left it before anything is written, which is where it is refused, as on the heap. */
int main(void) {
    int local[16];
    int i = 0;
    while (i < 4) {
        i++;
    }
    local[-i] = 7;
    return 0;
}
