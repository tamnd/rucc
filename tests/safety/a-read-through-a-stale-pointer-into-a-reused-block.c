/* row: T1 */
/* refuse: J1 */
/* gap: #428 */
void *malloc(unsigned long size);
void free(void *p);
/* The half of use after free that an address alone cannot answer. The block is handed out again,
   so the plane says somebody owns it, and whether it is the somebody this pointer was made for is
   a question about the version the pointer carries. */
int main(void) {
    int *p = malloc(64);
    int *q;
    free(p);
    q = malloc(64);
    q[0] = 1;
    return p[0];
}
