/* row: S8 */
/* refuse: J1 */
/* says: in memcpy, over its dst argument */
void *memcpy(void *to, const void *from, unsigned long count);
void *memset(void *to, int byte, unsigned long count);
/* Juliet's CWE-121 alloca variants: fifty bytes from `__builtin_alloca` and a copy of a hundred
   into them. The buffer is a local whose size is a value, and the call to the wrapper hands over
   its capability like any other local's, so the copy is held to the fifty. */
int main(void) {
    char *data = __builtin_alloca(50);
    char source[100];
    memset(source, 'C', 99);
    source[99] = 0;
    memcpy(data, source, 100);
    return data[0];
}
