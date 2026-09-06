/* row: S8 */
/* refuse: J1 */
/* says: in strncpy, over its dst argument */
void *malloc(unsigned long size);
void free(void *p);
char *strncpy(char *to, const char *from, unsigned long count);
/* strncpy writes its count whatever the source holds, padding the rest with terminators, which is
   the part people forget when they reach for it as the safe one. Here the source is two characters
   and the overflow is written entirely by the padding. */
int main(void) {
    char *field = malloc(16);
    strncpy(field, "hi", 64);
    free(field);
    return 0;
}
