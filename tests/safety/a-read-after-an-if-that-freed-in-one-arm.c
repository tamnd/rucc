/* row: T1 */
/* refuse: J1 */
/* says: which has been freed */
void *malloc(unsigned long size);
void free(void *p);
/* A read in front of an `if`, a free in one arm, and the same read after the join. The read in
   front dominates the one after, so a walk down the dominator tree reaches the second straight
   from the first and never through the arm, and the lifetime it found alive was taken as still
   alive. The flag is volatile so no pass can decide which way the branch goes. */
volatile int release = 1;

int main(void) {
    int *p = malloc(64);
    int first;
    p[0] = 7;
    first = p[0];
    if (release) {
        free(p);
    }
    return first + p[0];
}
