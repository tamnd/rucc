/* row: S8 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
void *memcpy(void *to, const void *from, unsigned long count);
/* The other half of the wrapper's job. A boundary check that refuses the copies that fit is
   worse than no boundary check, because it is the one people turn off. */
int main(void) {
    char *from = malloc(16);
    char *to = malloc(16);
    int i;
    for (i = 0; i < 16; i++) {
        from[i] = (char)i;
    }
    memcpy(to, from, 16);
    if (to[15] != 15) {
        return 1;
    }
    free(from);
    free(to);
    return 0;
}
