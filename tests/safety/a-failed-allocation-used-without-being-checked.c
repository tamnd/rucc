/* row: S6 */
/* refuse: J1 */
/* gap: #431 */
void *malloc(unsigned long size);
/* The request is large enough that the allocator says no, and the program does what a great deal
   of C does with that answer, which is not look at it. */
int main(void) {
    int *p = malloc(0xffffffffffff0000UL);
    p[0] = 1;
    return 0;
}
