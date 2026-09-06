/* The other scalar shape, which is a byte at a time over one buffer. */
/* Tokenising, parsing and every strlen in the program look like this: a single cursor walking
   forward, one byte at a time, with a branch per byte. It is the cheapest possible thing to
   check, one live capability and a monotone address, so it is the row where any overhead at all
   is a fact about how the check is emitted rather than about the workload. */
void *malloc(unsigned long size);
void free(void *p);

int main(void) {
    char *text = malloc(1 << 20);
    long words = 0;
    long i;
    int round;
    for (i = 0; i < (1 << 20); i++) {
        text[i] = (char)((i % 7) == 0 ? ' ' : 'a');
    }
    text[(1 << 20) - 1] = 0;
    for (round = 0; round < 40; round++) {
        char *cursor = text;
        int inside = 0;
        while (*cursor) {
            if (*cursor == ' ') {
                inside = 0;
            } else if (!inside) {
                inside = 1;
                words++;
            }
            cursor++;
        }
    }
    free(text);
    return words == 0 ? 1 : 0;
}
