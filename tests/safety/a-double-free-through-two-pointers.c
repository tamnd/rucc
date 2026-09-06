/* row: T2 */
/* refuse: J6 */
void *malloc(unsigned long size);
void free(void *p);
/* Two owners who each think they are the only one, which is what a double free is in every
   program large enough to have one. Neither call site looks wrong on its own. */
int main(void) {
    int *first = malloc(64);
    int *second = first;
    free(first);
    free(second);
    return 0;
}
