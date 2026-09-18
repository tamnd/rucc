/* row: T1 */
/* refuse: J1 */
/* says: which has been freed */
void *malloc(unsigned long size);
void free(void *p);
/* The same assignment the other way round, and the half that is quiet rather than loud. The
   destination already holds a pointer to something live, so the slot beside it describes a live
   instance, and a copy that moves the bytes and leaves the slot alone leaves that description
   sitting beside a pointer to storage that has been freed. The read below then asks a capability
   that is about the wrong instance and is told the program is fine. Carrying the slots across is
   what makes it a use after free again, which is why the two halves of tamnd/rucc#1471 are one
   change. */
struct band {
    int *values;
    int count;
    int width;
};

int main(void) {
    struct band *one = malloc(sizeof *one);
    struct band *two = malloc(sizeof *two);
    int *kept = malloc(4 * sizeof(int));
    int *gone = malloc(4 * sizeof(int));

    kept[0] = 1;
    gone[0] = 2;
    one->values = kept;
    one->count = 1;
    one->width = 1;
    two->values = gone;
    two->count = 1;
    two->width = 1;
    free(gone);

    *one = *two;

    return one->values[0];
}
