/* row: T6 */
/* refuse: J1 */
/* gap: #431 */
/* Mappings are storage instances the same way allocations are, and the allocator interposition
   API is what tells the runtime that one began and ended. Until mmap and munmap are wrapped this
   memory is outside the heap the monitor watches and its unmapping is invisible. */
void *mmap(void *at, unsigned long length, int protection, int flags, int fd, long offset);
int munmap(void *at, unsigned long length);

int main(void) {
    char *page = mmap(0, 4096, 3, 0x22, -1, 0);
    page[0] = 7;
    munmap(page, 4096);
    return page[0];
}
