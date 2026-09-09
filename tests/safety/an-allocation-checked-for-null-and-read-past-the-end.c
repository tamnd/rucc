/* row: S1 */
/* refuse: J1 */
/* Testing the answer for null says the pointer is a pointer and says nothing about how far it
   goes. The optimizer reads the extent off the request, this access is a long way past it, and the
   check the test does not license stays behind to refuse it. */
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
