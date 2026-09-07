/* row: S5 */
/* refuse: J2 */
void *malloc(unsigned long size);
void free(void *p);
/* The other end. Document 03 section 3.1 opens the window by exactly one element, because
   &a[-1] is how a great deal of working C is written, and two elements is a loop that ran too
   far rather than an idiom anybody meant to write. */
int main(void) {
    int *p = malloc(64);
    int *q = p - 2;
    return *q;
}
