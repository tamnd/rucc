/* row: S7 */
/* refuse: J1 */
/* gap: #428 */
void *malloc(unsigned long size);
void free(void *p);
/* An int read one byte into an allocation. The machine allows it and the standard does not, and
   catching it needs the access alignment beside the check, which the capability carries. */
int main(void) {
    char *p = malloc(64);
    int *q = (int *)(p + 1);
    return *q;
}
