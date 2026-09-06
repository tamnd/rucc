/* A scalar workload, for contrast. */
/* Dense array indexing with everything in cache and the same three pointers used a million
   times. If the monitor is expensive here, it is expensive because of the instructions it adds
   rather than the memory traffic, and the two are the ones document 13 section 13.1 says are
   worth telling apart. This is also the row that check elimination should eventually take to
   nothing, so it is the one to look at again at S4. */
void *malloc(unsigned long size);
void free(void *p);

#define N 200

int main(void) {
    long *a = malloc(N * N * sizeof(long));
    long *b = malloc(N * N * sizeof(long));
    long *c = malloc(N * N * sizeof(long));
    int i;
    int j;
    int k;
    for (i = 0; i < N * N; i++) {
        a[i] = i;
        b[i] = i + 1;
        c[i] = 0;
    }
    for (i = 0; i < N; i++) {
        for (j = 0; j < N; j++) {
            long sum = 0;
            for (k = 0; k < N; k++) {
                sum += a[i * N + k] * b[k * N + j];
            }
            c[i * N + j] = sum;
        }
    }
    i = (int)c[N * N - 1];
    free(a);
    free(b);
    free(c);
    return i == 0 ? 1 : 0;
}
