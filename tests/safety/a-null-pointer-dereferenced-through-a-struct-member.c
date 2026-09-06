/* row: S6 */
/* refuse: J1 */
/* gap: #431 */
/* The member offset means the faulting address is not zero, which is why a null check written as
   a comparison against the low page does not catch a big enough struct. Provenance does. */
struct big {
    char padding[8192];
    int field;
};

int main(void) {
    struct big *b = 0;
    return b->field;
}
