/* row: T6 */
/* refuse: J1 */
/* A mapping is a storage instance the way an allocation is. The runtime places it in an arena of
   its own, so the plane covers it, and munmap ends the instance there and keeps the range, so the
   read below is refused before it can fault or reach whatever the kernel mapped there since. */
void *mmap(void *at, unsigned long length, int protection, int flags, int fd, long offset);
int munmap(void *at, unsigned long length);

int main(void) {
    char *page = mmap(0, 4096, 3, 0x22, -1, 0);
    page[0] = 7;
    munmap(page, 4096);
    return page[0];
}
