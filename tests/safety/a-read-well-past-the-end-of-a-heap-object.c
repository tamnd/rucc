/* row: S5 */
/* refuse: J2 */
void *malloc(unsigned long size);
void free(void *p);
/* Caught where the address is computed rather than where it is read, because a pointer this far
   out has left its object and that is a judgement about the arithmetic. */
int main(void) {
    int *p = malloc(64);
    return p[64];
}
