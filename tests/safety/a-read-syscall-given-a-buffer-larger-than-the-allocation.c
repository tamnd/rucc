/* row: S9 */
/* refuse: J1 */
/* says: in read, over its buf argument */
void *malloc(unsigned long size);
void free(void *p);
long read(int fd, void *to, unsigned long count);
/* The kernel writes past the end and no instrumented instruction was involved, so the only place
   the extent can be compared to the count is the syscall wrapper. S9 is the row that decides
   whether the boundary story is complete or only covers libc. */
int main(void) {
    char *small = malloc(16);
    read(0, small, 4096);
    free(small);
    return 0;
}
