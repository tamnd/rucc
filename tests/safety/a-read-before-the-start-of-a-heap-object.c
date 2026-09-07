/* row: S1 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *p);
/* Underflow is the same bug facing the other way, and it is the one that reads the allocator's
   own bookkeeping rather than the next program object. The address is one element below the
   start, which document 04's J2 now permits a program to compute, so this is the case that says
   the widened window did not widen J1: computing &p[-1] is allowed and reading through it is
   not. */
int main(void) {
    int *p = malloc(64);
    int seen = p[-1];
    free(p);
    return seen;
}
