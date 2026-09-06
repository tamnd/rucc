/* row: 3.5 low bit tagged pointers */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Alignment leaves the low bits free and a great deal of C keeps a flag in them. The tagged
   value is never read through, only the untagged one is. */
int main(void) {
    int *p = malloc(64);
    unsigned long tagged = (unsigned long)p | 1UL;
    int *q = (int *)(tagged & ~1UL);
    int read;
    q[0] = 9;
    read = q[0];
    free(p);
    return read == 9 ? 0 : 1;
}
