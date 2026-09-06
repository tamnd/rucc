/* row: S5 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Pointer difference is how a length is recovered from a pair of cursors, and neither operand
   moves anywhere, so a checker that fires here fires on every parser ever written. */
int main(void) {
    char *p = malloc(64);
    char *start = p + 8;
    char *end = p + 40;
    long length = end - start;
    free(p);
    return length == 32 ? 0 : 1;
}
