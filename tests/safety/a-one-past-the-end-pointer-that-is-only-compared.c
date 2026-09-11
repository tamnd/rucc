/* row: 3.5 one past the end */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* C promises the address just past the last element exists and may be compared against. It may
   not be read through, and this program does not read through it. */
int main(void) {
    int *p = malloc(64);
    int *end = p + 16;
    int *q;
    int sum = 0;
    /* The elements are written before they are added up, because the row here is about where the
       walk stops and not about what the storage held before it started. */
    for (q = p; q < end; q++) {
        *q = 1;
    }
    for (q = p; q < end; q++) {
        sum += *q;
    }
    free(p);
    return sum == 16 ? 0 : 1;
}
