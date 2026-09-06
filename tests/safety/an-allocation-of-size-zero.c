/* row: T3 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* glibc returns a real, unique, freeable pointer for a zero byte request, and code that appends
   to an empty list relies on it. Nothing may be read through it and it still has to be freeable,
   which makes it the one allocation whose extent is genuinely nothing. */
int main(void) {
    void *p = malloc(0);
    if (p == 0) {
        return 1;
    }
    free(p);
    return 0;
}
