/* row: S2 */
/* refuse: J1 */
/* says: in strcpy, over its dst argument */
char *strcpy(char *to, const char *from);
/* Juliet's flow variant 12: a coin picks the buffer that is too small or the one that is not, so
   the pointer the copy is handed is one of two locals and carries the bounds of the one it is. */
int main(int argc, char **argv) {
    char *data;
    char small[10];
    char large[11];
    char source[11] = "AAAAAAAAAA";
    (void)argv;
    if (argc > 5)
        data = large;
    else
        data = small;
    data[0] = 0;
    strcpy(data, source);
    return data[0] == 'A' ? 0 : 1;
}
