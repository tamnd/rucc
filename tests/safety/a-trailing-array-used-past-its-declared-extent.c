/* row: S4 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* The `T x[1]` idiom that predates flexible array members, still in every protocol parser
   written before 1999. Section 3.5 says the bounds come from the allocation and not the declared
   type, so eight elements out of a one element declaration is exactly right. */
struct message {
    int length;
    int payload[1];
};

int main(void) {
    struct message *m = malloc(sizeof(struct message) + 7 * sizeof(int));
    int i;
    m->length = 8;
    for (i = 0; i < 8; i++) {
        m->payload[i] = i;
    }
    if (m->payload[7] != 7) {
        return 1;
    }
    free(m);
    return 0;
}
