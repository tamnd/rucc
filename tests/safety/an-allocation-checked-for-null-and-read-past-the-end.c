/* row: S1 */
/* refuse: J2 */
/* Testing the answer for null says the pointer is a pointer and says nothing about how far it
   goes. Four elements of room and an index of sixteen, so the check the test does not license
   stays behind to refuse it. The report is J2 rather than J1 because a pointer that far out has
   left its object before anything is stored through it, which is where section 4.4 of
   spec/safe-memory/04-safety-model.md puts a derivation violation and is what the two
   well-past-the-end cases next to this one already expect. */
void *malloc(unsigned long size);
void free(void *p);
int main(void) {
    int *p = malloc(4 * sizeof(int));
    if (p == 0) {
        return 0;
    }
    p[16] = 1;
    free(p);
    return 0;
}
