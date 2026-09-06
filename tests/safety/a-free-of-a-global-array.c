/* row: T3 */
/* refuse: J6 */
void *malloc(unsigned long size);
void free(void *p);
/* A pointer that sometimes came from malloc and sometimes points at a static buffer, freed on
   both paths. The static case is not an allocation base and never was. */
int table[16];

int main(void) {
    free(table);
    return 0;
}
