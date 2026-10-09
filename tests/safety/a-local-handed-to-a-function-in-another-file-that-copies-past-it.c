/* row: S8 */
/* refuse: J1 */
/* says: in memcpy, over its dst argument */
/* with: fill */
void *memset(void *to, int byte, unsigned long count);
void fill(char *to, const char *from, unsigned long count);
/* Juliet's CWE-121 copy with the copy moved into another file. Nothing in this file defines
   `fill`, and a call to a function this unit does not define used to clear the frame, so the copy
   in the other file saw a pointer with no bounds and wrote fifty bytes past the local. The frame
   now says which function it is for and carries the capability of `data` across. */
int main(void) {
    char data[50];
    char source[100];
    memset(source, 'C', 99);
    source[99] = 0;
    fill(data, source, 100);
    return data[0];
}
