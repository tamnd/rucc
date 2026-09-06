/* row: 3.5 pointer to integer round trip */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Every hash table of pointers does this. The address is exposed by the cast out and recovered
   by the cast back, and the instance it names is the one it always was. */
int main(void) {
    int *p = malloc(64);
    unsigned long bits = (unsigned long)p;
    int *q = (int *)bits;
    q[0] = 5;
    free(p);
    return q == p ? 0 : 1;
}
