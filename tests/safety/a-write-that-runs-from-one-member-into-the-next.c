/* row: S4 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Intra object overflow, which stays inside the allocation and only leaves the member. The table
   in section 3.3 marks S4 as opt in at every tier, so the default answer to this program is
   silence. `-fsafety-subobject` does not change the answer either, because both members are int
   and a write of an int over an int gives the type plane nothing to disagree with. Catching this
   one wants the strict reading of section 9.4, where a member is its own extent rather than its
   own type, and that is tamnd/rucc#967. */
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
