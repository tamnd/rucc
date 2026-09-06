/* Document 12 section 12.4's fourth addition, in the only form available at S1. */
/* Section 12.4 asks for a memcpy dominated loop, on the grounds that a bulk copy is where the
   checks should be nearly free, because one wrapper call checks a whole range. There are no
   boundary wrappers yet, so this is the same workload written the way a program without memcpy
   writes it: one check per byte, both sides. Reading it as the memcpy row would be wrong. It is
   the upper bound the wrapper has to beat, and the row to watch when the wrappers land. */
void *malloc(unsigned long size);
void free(void *p);

int main(void) {
    char *from = malloc(1 << 16);
    char *to = malloc(1 << 16);
    long i;
    int round;
    for (i = 0; i < (1 << 16); i++) {
        from[i] = (char)i;
    }
    for (round = 0; round < 200; round++) {
        for (i = 0; i < (1 << 16); i++) {
            to[i] = from[i];
        }
    }
    i = to[(1 << 16) - 1];
    free(from);
    free(to);
    return i == 0 ? 1 : 0;
}
