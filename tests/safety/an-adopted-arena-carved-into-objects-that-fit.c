/* row: 3.5 allocators that carve one allocation into many */
/* allow */
void *mmap(void *addr, unsigned long len, int prot, int flags, int fd, long off);
void *memset(void *to, int byte, unsigned long count);
void __rucc_alloc_adopt(void *base, unsigned long size, unsigned class);
void __rucc_alloc_split(void *base, unsigned long size, unsigned flags);
void __rucc_alloc_merge(void *base);
/* The same arena used the way it is meant to be. Two objects, each written to its own length, and
   both given back. A monitor that reported anything here would be one nobody switches on twice. */
int main(void) {
    char *arena = (char *)mmap(0, 65536, 3, 0x22, -1, 0);
    char *first;
    char *second;
    if ((long)arena == -1L) {
        return 1;
    }
    __rucc_alloc_adopt(arena, 65536, 2);
    first = arena;
    second = arena + 64;
    __rucc_alloc_split(first, 64, 0);
    __rucc_alloc_split(second, 64, 0);
    memset(first, 'a', 64);
    memset(second, 'b', 64);
    if (first[63] != 'a' || second[0] != 'b') {
        return 1;
    }
    __rucc_alloc_merge(first);
    __rucc_alloc_merge(second);
    return 0;
}
