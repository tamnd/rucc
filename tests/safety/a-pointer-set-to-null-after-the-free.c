/* row: T1 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* The defensive habit, and the point of the case is that holding a freed pointer, comparing it
   and overwriting it are all fine. Only reading through one is not. */
int main(void) {
    int *p = malloc(64);
    p[0] = 1;
    free(p);
    if (p == 0) {
        return 1;
    }
    p = 0;
    return p == 0 ? 0 : 1;
}
