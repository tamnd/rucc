/* row: S1 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Eight live instances at once, each freed in turn. The plane has to keep eight versions apart
   rather than one, which is the difference between a demo and a runtime. */
int main(void) {
    int **table = malloc(8 * sizeof(int *));
    int i;
    for (i = 0; i < 8; i++) {
        table[i] = malloc(sizeof(int));
        *table[i] = i;
    }
    for (i = 0; i < 8; i++) {
        if (*table[i] != i) {
            return 1;
        }
        free(table[i]);
    }
    free(table);
    return 0;
}
