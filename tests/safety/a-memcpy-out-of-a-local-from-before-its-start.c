/* row: S8 */
/* refuse: J1 */
/* says: in memcpy, over its src argument */
void *memcpy(void *to, const void *from, unsigned long count);
void *memset(void *to, int byte, unsigned long count);
/* Juliet's CWE-127, the read the other way round: a hundred bytes copied out of a local starting
   eight bytes before it. */
int main(void) {
    char data[100];
    char dest[100];
    memset(data, 'A', 99);
    data[99] = 0;
    memcpy(dest, data - 8, 100);
    return dest[99];
}
