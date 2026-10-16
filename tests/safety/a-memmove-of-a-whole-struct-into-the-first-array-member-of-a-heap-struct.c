/* row: S8 */
/* refuse: J1 */
/* says: in memmove, over its dst argument */
void *malloc(unsigned long size);
void free(void *p);
void *memmove(void *dst, const void *src, unsigned long n);
/* The row above with the structure on the heap, which is how half of Juliet's cases write it. The
   frame has nothing to say about a block the allocator handed out, so the wrapper holds the move
   to the member's sixteen bytes on the call site's word alone. */
struct pair {
    char first[16];
    void *second;
    void *third;
};

int main(void) {
    struct pair *p = malloc(sizeof(*p));
    int ok;
    if (p == 0) {
        return 1;
    }
    memmove(p->first, "0123456789abcdef0123456789abcde", sizeof(*p));
    ok = p->first[0] == '0';
    free(p);
    return ok ? 0 : 1;
}
