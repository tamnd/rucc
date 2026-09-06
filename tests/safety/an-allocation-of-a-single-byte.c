/* row: S1 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* One byte is the smallest thing the allocator hands out and the granule is sixteen, so the
   whole rounding up story has to not leak into what the program is allowed to touch. */
int main(void) {
    char *p = malloc(1);
    p[0] = 3;
    if (p[0] != 3) {
        return 1;
    }
    free(p);
    return 0;
}
