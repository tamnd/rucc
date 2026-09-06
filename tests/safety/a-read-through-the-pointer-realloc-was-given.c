/* row: T5 */
/* refuse: J1 */
/* says: which has been freed */
void *malloc(unsigned long size);
void free(void *p);
void *realloc(void *p, unsigned long size);
/* realloc ends the instance it was handed, so the old pointer is dangling even when everything
   looks like it worked. */
int main(void) {
    int *p = malloc(64);
    int *q = realloc(p, 256);
    q[0] = 1;
    return p[0];
}
