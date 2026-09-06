/* row: S5 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Every idiomatic pointer loop ends on the one past the end address and then does nothing with
   it. Coming back inside afterwards has to work too, since the pointer never lost its object. */
int main(void) {
    int *p = malloc(4 * sizeof(int));
    int *cursor = p;
    int i;
    for (i = 0; i < 4; i++) {
        p[i] = i;
    }
    while (cursor != p + 4) {
        cursor++;
    }
    cursor--;
    if (*cursor != 3) {
        return 1;
    }
    free(p);
    return 0;
}
