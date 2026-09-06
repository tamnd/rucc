/* row: S1 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Handing the second half of a buffer to something that does not know about the first half is
   ordinary, and the capability the second half carries is the whole allocation's. */
int main(void) {
    int *p = malloc(16 * sizeof(int));
    int *half = p + 8;
    int i;
    for (i = 0; i < 8; i++) {
        half[i] = i;
    }
    free(p);
    return 0;
}
