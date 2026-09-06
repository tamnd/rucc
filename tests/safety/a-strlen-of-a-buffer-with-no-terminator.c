/* row: S8 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *p);
void *memset(void *to, int byte, unsigned long count);
unsigned long strlen(const char *s);
/* The discovered extent, which is the half of document 10 section 10.3 that a length check cannot
   do. There is no count to compare against, so the walk is the check, and it stops at the byte
   that leaves the object rather than after the read has already happened. */
int main(void) {
    char *text = malloc(16);
    memset(text, 'a', 16);
    if (strlen(text) == 16) {
        return 1;
    }
    free(text);
    return 0;
}
