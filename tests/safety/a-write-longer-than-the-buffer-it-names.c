/* row: S9 */
/* refuse: J1 */
/* says: in write, over its buf argument */
void *malloc(unsigned long size);
void free(void *p);
void *memset(void *to, int byte, unsigned long count);
long write(int fd, const void *from, unsigned long count);
/* Document 03's S9, and the half of it that leaks. The store that runs off the end happens inside
   the kernel, where there is no instrumentation to catch it, so the only place this can be caught
   is the call, and whatever was next to the buffer is what goes out. */
int main(void) {
    char *line = malloc(16);
    memset(line, 'x', 16);
    write(1, line, 1024);
    free(line);
    return 0;
}
