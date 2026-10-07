/* row: S8 */
/* refuse: J1 */
/* says: in memcpy, over its dst argument */
void *memcpy(void *to, const void *from, unsigned long count);
void *memset(void *to, int byte, unsigned long count);
/* The copy that Juliet's CWE-121 is mostly made of: a hundred bytes into a local of fifty. No
   region covers the stack, so the wrapper's planes have nothing to say about it, and the call used
   to clear the frame in front of the wrapper as it does for anything this unit does not define. It
   publishes now, with the capability of the local, and that is what the copy is held to. */
int main(void) {
    char data[50];
    char source[100];
    memset(source, 'C', 99);
    source[99] = 0;
    memcpy(data, source, 100);
    return data[0];
}
