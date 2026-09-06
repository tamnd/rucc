/* row: S1 */
/* refuse: J1 */
/* says: which no instance owns */
void *malloc(unsigned long size);
void free(void *p);
/* The last element is p[15]. Reading p[16] is the smallest heap overflow there is, and the
   address it lands on is the next block's header, which belongs to nobody. */
int main(void) {
    int *p = malloc(64);
    return p[16];
}
