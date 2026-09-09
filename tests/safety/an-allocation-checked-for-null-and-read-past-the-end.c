/* row: S1 */
/* refuse: J2 */
/* Testing the answer for null says the pointer is a pointer and says nothing about how far it
   goes. The optimizer reads the extent off the request, this index is a long way past it, and the
   check the test does not license stays behind to refuse it. The refusal lands on the arithmetic
   rather than the write, the same way it does in a-read-well-past-the-end-of-a-heap-object.c,
   because a pointer this far out has left its object before anything reads through it. */
void *malloc(unsigned long size);
void free(void *p);
int main(void) {
    int *p = malloc(4 * sizeof(int));
    if (p == 0) {
        return 0;
    }
    p[16] = 1;
    free(p);
    return 0;
}
