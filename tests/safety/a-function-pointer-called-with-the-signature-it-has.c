/* row: Y4 */
/* allow */
/* A dispatch table, which is how C does polymorphism and which has to stay free. Every entry is
   called through a pointer and every call matches the function it names. */
int add(int a, int b) {
    return a + b;
}

int subtract(int a, int b) {
    return a - b;
}

int main(void) {
    int (*table[2])(int, int);
    int i;
    int total = 0;
    table[0] = add;
    table[1] = subtract;
    for (i = 0; i < 2; i++) {
        total += table[i](10, 4);
    }
    return total == 20 ? 0 : 1;
}
