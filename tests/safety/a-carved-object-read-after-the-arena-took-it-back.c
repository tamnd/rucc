/* row: T1 */
/* refuse: J1 */
void *mmap(void *addr, unsigned long len, int prot, int flags, int fd, long off);
void *memset(void *to, int byte, unsigned long count);
void __rucc_alloc_adopt(void *base, unsigned long size, unsigned class);
void __rucc_alloc_split(void *base, unsigned long size, unsigned flags);
void __rucc_alloc_merge(void *base);
/* Use after free inside somebody else's heap, which is the half of temporal safety an arena gets
   for free once it says when its objects begin and end. The merge is the allocator taking the
   storage back, and the read afterwards goes through a pointer to an instance that is over. */
int main(void) {
    char *arena = (char *)mmap(0, 65536, 3, 0x22, -1, 0);
    char *object;
    if ((long)arena == -1L) {
        return 1;
    }
    __rucc_alloc_adopt(arena, 65536, 2);
    object = arena + 128;
    __rucc_alloc_split(object, 64, 0);
    memset(object, 'a', 64);
    __rucc_alloc_merge(object);
    return object[0];
}
