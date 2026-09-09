/* row: S1 */
/* refuse: J1 */
/* Testing the answer for null says the pointer is a pointer and says nothing about how far it
   goes. The optimizer reads the extent off the request, this access is one element past it, and
   the check the test does not license stays behind to refuse it.

   One element past and not sixteen, because sixteen is far enough out that forming the pointer is
   already wrong and the derivation check refuses it first. That is a refusal of a different
   judgement, and this file is about the bounds check that the null test did not license. */
void *malloc(unsigned long size);
void free(void *p);
int main(void) {
    int *p = malloc(4 * sizeof(int));
    if (p == 0) {
        return 0;
    }
    p[4] = 1;
    free(p);
    return 0;
}
