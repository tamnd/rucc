/* row: S8 */
/* refuse: J2 */
void *memcpy(void *to, const void *from, unsigned long count);
void *memset(void *to, int byte, unsigned long count);
/* Juliet's CWE-127, the read the other way round: a hundred bytes copied out of a local starting
   eight bytes before it. `data - 8` has already left the local, so it is refused before the copy
   is made. */
int main(void) {
    char data[100];
    char dest[100];
    memset(data, 'A', 99);
    data[99] = 0;
    memcpy(dest, data - 8, 100);
    return dest[99];
}
