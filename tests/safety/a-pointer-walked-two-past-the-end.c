/* row: S5 */
/* refuse: J2 */
/* gap: #491 */
void *malloc(unsigned long size);
void free(void *p);
/* One past the end is a real address in C and two past it is not. The difference is a single
   element and the whole point of S5 is that the line is drawn there rather than approximately.
   Written in one step this is refused. Written in two it is not, because the second derivation
   looks its base up in the plane and the one past the end address belongs to no instance, which
   is #491 and is the shape every pointer loop that runs one iteration too far has. */
int main(void) {
    int *p = malloc(4 * sizeof(int));
    int *end = p + 4;
    int *past = end + 1;
    free(p);
    return past == 0 ? 1 : 0;
}
