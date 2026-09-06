/* row: T1 */
/* refuse: J1 */
/* says: which has been freed */
void *malloc(unsigned long size);
void free(void *p);
/* Two names for the same block, freed through one and read through the other. This is the case
   a checker that nulls the pointer at the free site cannot see, and lock and key can. */
int main(void) {
    int *owner = malloc(64);
    int *alias = owner;
    owner[0] = 5;
    free(owner);
    return alias[0];
}
