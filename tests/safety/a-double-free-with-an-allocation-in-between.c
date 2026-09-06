/* row: T2 */
/* refuse: J6 */
/* gap: #492 */
void *malloc(unsigned long size);
void free(void *p);
/* The allocator may well have handed the block straight back out, so the second free is a free
   of somebody else's live object. Versions tell those two apart and addresses do not, and the
   free path only has the address today, which is #492. This is the worse half of a double free:
   the program keeps running and the object that gets reported later is the innocent one. */
int main(void) {
    int *p = malloc(64);
    int *other;
    free(p);
    other = malloc(64);
    other[0] = 1;
    free(p);
    return 0;
}
