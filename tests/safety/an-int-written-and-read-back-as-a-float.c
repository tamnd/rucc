/* row: Y2 */
/* refuse: J1 */
/* gap: #431 */
void *malloc(unsigned long size);
void free(void *p);
/* Strict aliasing, broken the usual way: two pointers of unrelated types to the same bytes. The
   compiler is allowed to assume this cannot happen, which is why the bug shows up as a wrong
   answer at high optimization rather than as a crash. The init and type planes are S5. */
int main(void) {
    void *raw = malloc(sizeof(float));
    int *as_int = raw;
    float *as_float = raw;
    *as_int = 1078530011;
    if (*as_float < 3.0f) {
        return 1;
    }
    free(raw);
    return 0;
}
