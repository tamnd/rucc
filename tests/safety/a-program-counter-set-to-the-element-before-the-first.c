/* row: S5 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* SQLite's bytecode interpreter, in miniature. The program counter starts one element before the
   first instruction and the top of the loop steps it on before anything reads through it, so the
   out of range address exists for the length of one increment and is never dereferenced. C says
   computing it is undefined and document 03 section 3.1 permits it anyway, with the reasons and
   the cost written there. This is the case that found the rule: the first full run of SQLite's
   test suite at Tier D stopped here. */
int main(void) {
    int *ops = malloc(8 * sizeof(int));
    int *pc = ops - 1;
    int sum = 0;
    int i;
    for (i = 0; i < 8; i++) {
        ops[i] = i;
    }
    for (i = 0; i < 8; i++) {
        pc++;
        sum += *pc;
    }
    free(ops);
    return sum == 28 ? 0 : 1;
}
