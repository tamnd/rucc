/* row: T3 */
/* refuse: J6 */
void *malloc(unsigned long size);
void free(void *p);
/* Nothing allocated this, so there is nothing to give back. */
int main(void) {
    int local[16];
    local[0] = 1;
    free(local);
    return 0;
}
