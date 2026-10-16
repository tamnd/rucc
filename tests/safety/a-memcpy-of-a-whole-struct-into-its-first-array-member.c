/* row: S8 */
/* refuse: J1 */
/* says: in memcpy, over its dst argument */
void *memcpy(void *dst, const void *src, unsigned long n);
/* Juliet's char_type_overrun_memcpy. The copy is the size of the whole structure and it is aimed
   at the array at the front of it, so it stays inside the local and runs over the two pointers
   after the array. The call site tells the wrapper the destination is a sixteen byte member, which
   is what `_FORTIFY_SOURCE` knows about the same call, and the wrapper holds the copy to that. */
struct pair {
    char first[16];
    void *second;
    void *third;
};

int main(void) {
    struct pair p;
    p.second = &p;
    memcpy(p.first, "0123456789abcdef0123456789abcde", sizeof(p));
    return p.first[0] == '0' ? 0 : 1;
}
