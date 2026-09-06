/* row: S2 */
/* refuse: J1 */
/* gap: #431 */
/* Underflow of a local, which on a downward growing stack walks into the caller's frame rather
   than out of the program. Same reason as the overflow: automatic storage has no instance. */
int main(void) {
    int local[16];
    int i = 0;
    while (i < 4) {
        i++;
    }
    local[-i] = 7;
    return 0;
}
