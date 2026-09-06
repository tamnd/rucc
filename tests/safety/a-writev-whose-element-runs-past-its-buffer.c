/* row: S9 */
/* refuse: J1 */
/* says: in writev, over its iov argument */
void *malloc(unsigned long size);
void free(void *p);
void *memset(void *to, int byte, unsigned long count);
struct iovec {
    void *base;
    unsigned long len;
};
long writev(int fd, const struct iovec *iov, int count);
/* One pointer argument that reaches a whole tree. The array is judged and then every buffer it
   names, so a bug in any one element is the bug, and here the first element is right and the
   second one is not. */
int main(void) {
    char *first = malloc(16);
    char *second = malloc(16);
    struct iovec iov[2];
    memset(first, 'a', 16);
    memset(second, 'b', 16);
    iov[0].base = first;
    iov[0].len = 16;
    iov[1].base = second;
    iov[1].len = 1024;
    writev(1, iov, 2);
    free(first);
    free(second);
    return 0;
}
