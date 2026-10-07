/* row: S2 */
/* refuse: J1 */
/* says: in strcpy, over its dst argument */
char *strcpy(char *to, const char *from);
/* Juliet's CWE-121 CWE193 cases at flow variants 05 and 09 to 14: the buffer is chosen under a test
   the optimizer cannot fold, and the other side of the test leaves the pointer unset. Once the
   optimizer has run the pointer arrives at the copy as either the buffer or a null, and the wrapper
   used to be handed no capability for it at all, so the copy one byte past the end went through. */
int main(int argc, char **argv) {
    char *data;
    char buffer[10];
    char source[11] = "AAAAAAAAAA";
    (void)argv;
    if (argc > 0) {
        data = buffer;
        data[0] = 0;
    }
    strcpy(data, source);
    return data[0] == 'A' ? 0 : 1;
}
