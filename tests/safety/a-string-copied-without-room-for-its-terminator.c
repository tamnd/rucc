/* row: S1 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *p);
/* Room was counted for the characters and not for the byte that ends them, so the terminator
   goes one past the end. Whole classes of CVEs are this and nothing more, and it is the
   seventeen byte case again: five bytes come out of a whole granule and the sixth is inside the
   block the allocator rounded up to, so only the header knows it was not asked for. */
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
