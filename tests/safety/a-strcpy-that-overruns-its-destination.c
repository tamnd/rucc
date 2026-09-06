/* row: S8 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *p);
char *strcpy(char *to, const char *from);
/* The function that has its own CVE class. The wrapper has to know the destination's extent and
   the length of the source, and neither is visible from the call site, so the judgement walks the
   source and judges the destination as it goes and stops at the byte that leaves it. */
int main(void) {
    char *to = malloc(8);
    strcpy(to, "a string that does not fit in eight bytes");
    free(to);
    return 0;
}
