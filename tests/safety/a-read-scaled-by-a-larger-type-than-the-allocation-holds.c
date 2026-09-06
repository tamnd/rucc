/* row: S1 */
/* refuse: J2 */
void *malloc(unsigned long size);
void free(void *p);
/* The count was right for the type it was written for and the pointer was later changed to a
   wider one. Sixteen elements of four bytes is not sixteen elements of eight, and the index that
   was the last valid one is now twice as far out as the object goes. */
int main(void) {
    long *p = malloc(16 * sizeof(int));
    long seen = p[15];
    free(p);
    return (int)seen;
}
