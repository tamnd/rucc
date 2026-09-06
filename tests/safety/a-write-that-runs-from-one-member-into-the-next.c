/* row: S4 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Intra object overflow, which stays inside the allocation and only leaves the member. The table
   in section 3.3 marks S4 as opt in at every tier, so the default answer to this program is
   silence and the day `-fsafety-subobject` exists it gets a case of its own. */
struct record {
    int name[4];
    int id;
};

int main(void) {
    struct record *r = malloc(sizeof(struct record));
    int i;
    r->id = 0;
    for (i = 0; i < 5; i++) {
        r->name[i] = i;
    }
    free(r);
    return 0;
}
