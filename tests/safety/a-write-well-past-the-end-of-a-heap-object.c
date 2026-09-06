/* row: S1 */
/* refuse: J2 */
void *malloc(unsigned long size);
void free(void *p);
/* The overflow that corrupts something rather than a header. Sixty four bytes of room and a
   write a kilobyte out lands in whatever the allocator handed to somebody else. The report comes
   from the arithmetic rather than the store, since a pointer that far out has left its object
   before anything is written through it. */
int main(void) {
    int *p = malloc(64);
    p[256] = 1;
    free(p);
    return 0;
}
