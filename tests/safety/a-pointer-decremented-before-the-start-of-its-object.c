/* row: S5 */
/* refuse: J2 */
void *malloc(unsigned long size);
void free(void *p);
/* A backwards loop written with a pointer instead of an index. The last decrement produces the
   address before the object, and C has no one before the start rule to match the one past the
   end one, so this is refused where the forward version is not. */
int main(void) {
    int *p = malloc(16 * sizeof(int));
    int *cursor = p + 15;
    int sum = 0;
    while (cursor >= p) {
        sum += *cursor;
        cursor--;
    }
    free(p);
    return sum;
}
