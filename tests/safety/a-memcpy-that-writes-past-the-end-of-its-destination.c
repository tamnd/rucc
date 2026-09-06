/* row: S8 */
/* refuse: J1 */
/* gap: #428 */
void *malloc(unsigned long size);
void free(void *p);
void *memcpy(void *to, const void *from, unsigned long count);
/* The overflow happens inside libc, where there is no instrumentation and never will be, so the
   only place to catch it is the wrapper that document 10 puts at the boundary. */
int main(void) {
    char *from = malloc(64);
    char *to = malloc(16);
    memcpy(to, from, 64);
    free(from);
    free(to);
    return 0;
}
