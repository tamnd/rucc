/* row: S8 */
/* refuse: J1 */
/* says: in memcpy, over its dst argument */
/* with: fill */
void *memset(void *to, int byte, unsigned long count);
void fill(char *to, const char *from, unsigned long count);
/* The copy past the local again, with the call made through a pointer the compiler cannot see
   through. The frame is written for whatever the pointer holds, and `fill` takes it because that
   is its own address. */
void (*volatile through)(char *, const char *, unsigned long) = fill;
int main(void) {
    char data[50];
    char source[100];
    memset(source, 'C', 99);
    source[99] = 0;
    through(data, source, 100);
    return data[0];
}
