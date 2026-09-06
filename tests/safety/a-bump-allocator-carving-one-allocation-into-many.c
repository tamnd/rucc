/* row: 3.5 allocators that carve one allocation into many */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Section 3.5 says a real allocator carves one storage instance into many, and this is the two
   hundred line version of that. Without the split API of document 10 it is still one instance,
   so every carved pointer carries the arena's capability and none of this is a violation. */
int main(void) {
    char *arena = malloc(1024);
    char *bump = arena;
    int *first;
    int *second;
    int i;
    first = (int *)bump;
    bump += 16 * sizeof(int);
    second = (int *)bump;
    bump += 16 * sizeof(int);
    for (i = 0; i < 16; i++) {
        first[i] = i;
        second[i] = i * 2;
    }
    if (first[15] != 15 || second[15] != 30) {
        return 1;
    }
    free(arena);
    return 0;
}
