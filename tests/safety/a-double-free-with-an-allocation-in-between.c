/* row: T2 */
/* refuse: J6 */
void *malloc(unsigned long size);
void free(void *p);
/* The allocator may well have handed the block straight back out, so the second free is a free
   of somebody else's live object. Versions tell those two apart and addresses do not, and the
   check in front of the free is what asks the version question. This is the worse half of a
   double free: without it the program keeps running and the object that gets reported later is
   the innocent one. */
int main(void) {
    int *p = malloc(64);
    int *other;
    free(p);
    other = malloc(64);
    other[0] = 1;
    free(p);
    return 0;
}
