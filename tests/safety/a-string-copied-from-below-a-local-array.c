/* row: S5 */
/* refuse: J2 */
char *strcpy(char *to, const char *from);
void *memset(void *to, int byte, unsigned long count);
/* Juliet's CWE-127 strcpy: the source is eight bytes below a local array. Whatever is in those
   bytes decides how far the copy goes, and when the first of them is zero it copies an empty
   string and nothing it reads reaches the array, so the walk alone never said anything. The
   pointer was already outside the array when it was made, which is where it is refused. */
int main(void) {
    char data[100];
    char dest[100];
    memset(data, 'A', 99);
    data[99] = 0;
    char *from = data - 8;
    strcpy(dest, from);
    return dest[0] == 'A' ? 0 : 1;
}
