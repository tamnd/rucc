/* row: S1 */
/* refuse: J1 */
/* The null check again, with the access at exactly one past the end this time. Computing that
   address is something C permits and the model permits with it, so nothing refuses the
   arithmetic, and the store through it is left for the access judgement to refuse. That is the
   pairing the file next to this one used to cover, which it stopped covering when its own index
   turned out to be far enough out that the derivation judgement reaches it first. */
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
