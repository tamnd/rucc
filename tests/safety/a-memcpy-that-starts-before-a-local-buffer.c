/* row: S8 */
/* refuse: J2 */
void *memcpy(void *to, const void *from, unsigned long count);
void *memset(void *to, int byte, unsigned long count);
/* Juliet's CWE-124: the destination is eight bytes before a local, and the copy runs from there on
   into it. The pointer is outside the local from the moment it is made, and the capability the
   compiler made for the local says so, so it is refused there rather than in the copy. */
int main(void) {
    char data[100];
    char source[100];
    char *start = data - 8;
    memset(source, 'C', 99);
    source[99] = 0;
    memcpy(start, source, 100);
    return data[0];
}
