/* row: S8 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
void *memcpy(void *to, const void *from, unsigned long count);
int strncmp(const char *a, const char *b, unsigned long count);
/* The reason the bounded extent has to be its own thing. A fixed width field with no room for a
   terminator is the whole reason strncmp exists, and judging the walk as if it ran to a NUL would
   refuse correct code. */
int main(void) {
    char *field = malloc(8);
    char *other = malloc(8);
    memcpy(field, "abcdefgh", 8);
    memcpy(other, "abcdefgh", 8);
    if (strncmp(field, other, 8) != 0) {
        return 1;
    }
    free(field);
    free(other);
    return 0;
}
