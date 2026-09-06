/* row: S2 */
/* refuse: J1 */
/* gap: #431 */
/* A stack overflow that writes, which is the one that reaches the return address. Nothing marks
   an automatic instance as live yet, so the address belongs to no instance and the check has
   nothing to compare against. */
int main(void) {
    int local[16];
    local[16] = 1;
    return local[0];
}
