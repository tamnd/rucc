/* row: S8 */
/* refuse: J1 */
/* says: in strcpy, over its dst argument */
void *malloc(unsigned long size);
void free(void *p);
char *strcpy(char *to, const char *from);
/* Room for the ten characters and not for the terminator, through `strcpy` this time rather than
   through a loop. Ten bytes come out of a whole granule, so the plane says the eleventh is the
   block's too, and the walk the wrapper does used to ask only the plane. It asks the header now,
   which says ten. Juliet's CWE-122 has a whole family of these. */
int main(void) {
    char *to = malloc(10);
    strcpy(to, "AAAAAAAAAA");
    free(to);
    return 0;
}
