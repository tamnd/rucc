/* row: Y1 */
/* refuse: J3 */
/* gap: #431 */
void *malloc(unsigned long size);
void free(void *p);
/* A word that was never a pointer, read as one and followed. This is the exploit primitive that
   every heap grooming attack ends with, and telling it apart from a legitimate round trip needs
   the type plane, which is milestone S5. */
int main(void) {
    void **slot = malloc(sizeof(void *));
    int **as_pointer = (int **)slot;
    int *followed;
    *(long *)slot = 0x4141414141414141L;
    followed = *as_pointer;
    free(slot);
    return *followed;
}
