/* row: S4 */
/* flags: -fsafety-subobject */
/* refuse: J1 */
/* says: judgement J1 */
/* Intra object overflow caught by the type plane, which is what `-fsafety-subobject` turns on.
   The write leaves `name` and lands in `id`, and the bytes it lands on say they hold a double
   because the line above stored one through them, so the store is refused where the same write
   into untouched bytes would not be. The read half of this has been refused since the plane was
   written, because a read always asks. The store half is a flag because C 6.5 lets a program
   retype storage the allocator gave it, and the case next to this one is what that costs. */
void *malloc(unsigned long size);
void free(void *p);

struct record {
    int name[4];
    double id;
};

int main(void) {
    struct record *r = malloc(sizeof(struct record));
    int i;
    r->id = 1.5;
    for (i = 0; i < 5; i++) {
        r->name[i] = i;
    }
    free(r);
    return 0;
}
