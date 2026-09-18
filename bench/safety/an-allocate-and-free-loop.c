/* The allocation path on its own, which none of the other programs measure. */
/* Every other program here allocates once and then spends its time walking what it built, so the
   work an instance beginning does is amortized away by the time the ratio is taken. This one does
   almost nothing else: it allocates a block, writes one word of it so the block is real, frees it
   and does that again, which is what a program built around short lived objects looks like and is
   where the four per instance clears are paid for. The sizes vary so the free lists are all in use
   rather than one of them, since a block coming back off a list is the case that has to clear the
   aux and a block coming off the bump is not. */
void *malloc(unsigned long size);
void free(void *p);

#define ROUNDS 200000

int main(void) {
    static const unsigned long sizes[8] = { 24, 40, 64, 96, 136, 200, 312, 504 };
    unsigned long total = 0;
    int round;
    for (round = 0; round < ROUNDS; round++) {
        int which;
        for (which = 0; which < 8; which++) {
            char *block = malloc(sizes[which]);
            if (!block) {
                return 1;
            }
            block[0] = (char)which;
            block[sizes[which] - 1] = (char)round;
            total += (unsigned long)block[0];
            free(block);
        }
    }
    return total == 0 ? 1 : 0;
}
