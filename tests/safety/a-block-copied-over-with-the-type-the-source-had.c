/* row: S4 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
void *memcpy(void *to, const void *from, unsigned long size);
/* The other half of what a wrapper owes the type plane, and the one C writes down by name: a copy
   made with `memcpy` carries the source's effective type rather than making the destination bytes.
   So a block that held counts and was then copied over from an array of weights holds weights, and
   reading one back is what the program asked for. The wrapper used to say nothing about types at
   all, which left the block claiming to hold counts and refused the read. This is the idiom behind
   every hand rolled growable array, where the larger block is filled by a copy and then used as
   whatever the smaller one was. */
int main(void) {
    double *weights = malloc(64);
    void *block = malloc(64);
    int *counts = block;
    double *again;
    int answer;

    weights[0] = 2.5;
    counts[0] = 1;
    counts[1] = 2;
    memcpy(block, weights, 64);

    again = block;
    answer = again[0] > 2.0 ? 0 : 1;
    free(weights);
    free(block);
    return answer;
}
