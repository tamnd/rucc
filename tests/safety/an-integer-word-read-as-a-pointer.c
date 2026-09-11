/* row: Y1 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *p);
/* A word that was never a pointer, read as one and followed. This is the exploit primitive that
   every heap grooming attack ends with, and telling it apart from a legitimate round trip is what
   the type plane is for.

   J1 rather than J3, which is the judgement about an integer turned into a pointer. Nothing here
   asks about provenance, and what refuses is the type plane saying the bytes were stored through a
   long and are being read back as a pointer. J1's own wording is about an access the planes did
   not permit, so the report says the true thing about what was decided. */
int main(void) {
    void **slot = malloc(sizeof(void *));
    int **as_pointer = (int **)slot;
    int *followed;
    *(long *)slot = 0x4141414141414141L;
    followed = *as_pointer;
    free(slot);
    return *followed;
}
