/* row: S2 */
/* allow */
void *memcpy(void *to, const void *from, unsigned long count);
unsigned long strlen(const char *s);
/* The two kinds of local whose size is only known when the function runs, each written to its last
   byte and handed to a wrapper with exactly what fits, and a pointer into the middle of one walked
   to its end. None of it is out of bounds, so the capability made out of the operand has to say
   yes to all of it. */
static int sum(const char *at, int count) {
    int total = 0;
    int i;
    for (i = 0; i < count; i++) {
        total += at[i];
    }
    return total;
}

int run(int count) {
    char values[count];
    char *grown = __builtin_alloca(count);
    int i;
    for (i = 0; i < count - 1; i++) {
        values[i] = 'a';
    }
    values[count - 1] = 0;
    memcpy(grown, values, count);
    return (int)strlen(grown) + sum(values + count / 2, count - count / 2) + sum(grown, count);
}

int main(void) {
    return run(9) == 8 + 4 * 97 + 8 * 97 ? 0 : 1;
}
