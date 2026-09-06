/* row: S9 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
void *memset(void *to, int byte, unsigned long count);
struct iovec {
    void *base;
    unsigned long len;
};
long writev(int fd, const struct iovec *iov, int count);
/* The same tree with every range where it says it is, which is what a program gathering a header
   and a body into one call looks like when it got it right. */
int main(void) {
    char *first = malloc(16);
    char *second = malloc(16);
    struct iovec iov[2];
    memset(first, 'a', 16);
    memset(second, 'b', 15);
    second[15] = '\n';
    iov[0].base = first;
    iov[0].len = 16;
    iov[1].base = second;
    iov[1].len = 16;
    if (writev(1, iov, 2) != 32) {
        return 1;
    }
    free(first);
    free(second);
    return 0;
}
