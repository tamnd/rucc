/* row: S5 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* The same rule from the other direction, and the reason it is one rule rather than two. A walk
   that counts down and stops when the pointer passes the first element computes the address
   before the first element on its final step, which is the same address the interpreter above
   starts from. Neither reads through it. */
int main(void) {
    int *a = malloc(8 * sizeof(int));
    int *p;
    int sum = 0;
    int i;
    for (i = 0; i < 8; i++) {
        a[i] = i;
    }
    for (p = a + 7; p >= a; p--) {
        sum += *p;
    }
    free(a);
    return sum == 28 ? 0 : 1;
}
