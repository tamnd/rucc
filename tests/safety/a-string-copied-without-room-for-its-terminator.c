/* row: S1 */
/* refuse: J1 */
/* gap: #431 */
void *malloc(unsigned long size);
void free(void *p);
/* Room was counted for the characters and not for the byte that ends them, so the terminator
   goes one past the end. Whole classes of CVEs are this and nothing more, and this one is missed
   for the same reason the seventeen byte case is: five bytes come out of a whole granule and the
   sixth is inside the block the allocator rounded up to. */
int main(void) {
    const char *from = "hello";
    char *to = malloc(5);
    int i;
    for (i = 0; i < 5; i++) {
        to[i] = from[i];
    }
    to[5] = 0;
    free(to);
    return 0;
}
