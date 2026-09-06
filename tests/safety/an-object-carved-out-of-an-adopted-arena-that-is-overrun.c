/* row: S1 */
/* refuse: J1 */
/* says: in memset, over its dst argument */
void *mmap(void *addr, unsigned long len, int prot, int flags, int fd, long off);
void *memset(void *to, int byte, unsigned long count);
void __rucc_alloc_adopt(void *base, unsigned long size, unsigned class);
void __rucc_alloc_split(void *base, unsigned long size, unsigned flags);
/* An allocator that is not ours takes a region from the kernel and carves objects out of it, which
   is what every production allocator does and what document 10 section 10.4 is for. Without the
   split the whole arena is one instance and this overrun lands inside it, which is the case a
   monitor that does not know about carving cannot catch at all. With it the second object owns the
   granules the first one runs into, and the write is refused where it crosses. */
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
    memset(first, 'x', 80);
    return 0;
}
