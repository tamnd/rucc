/* row: S8 */
/* refuse: J1 */
/* says: in memcpy, over its dst argument */
void *memcpy(void *to, const void *from, unsigned long count);
void *memset(void *to, int byte, unsigned long count);
/* Juliet's CWE-124: the destination is eight bytes before a local, and the copy runs from there on
   into it. The pointer is outside the object the capability the caller handed over names, which
   used to be enough for the wrapper to stop believing it, but a range that reaches into an object
   from below has left wherever it started, so it is refused. */
int main(void) {
    char data[100];
    char source[100];
    char *start = data - 8;
    memset(source, 'C', 99);
    source[99] = 0;
    memcpy(start, source, 100);
    return data[0];
}
