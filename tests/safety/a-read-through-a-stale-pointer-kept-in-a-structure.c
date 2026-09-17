/* row: T1 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *p);
/* The same use after free as the stale pointer row next to this one, with the pointer living in
   memory rather than in a register across the free. That is the ordinary shape of it: a program
   big enough to have the bug keeps its pointers in structures. It needs the other half of the
   version rule, which is that a pointer written to memory has its capability written into the slot
   beside it and a pointer read back out takes the capability the slot holds. Recovering one from
   the address instead would find whatever instance owns those bytes now, which is the instance the
   second allocation made, and would pass. */
struct holder {
    int *at;
};
int main(void) {
    struct holder *h = malloc(sizeof(struct holder));
    int *p = malloc(64);
    int *q;
    h->at = p;
    free(p);
    q = malloc(64);
    q[0] = 1;
    return h->at[0];
}
