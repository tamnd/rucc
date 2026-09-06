/* row: T3 */
/* refuse: J6 */
void *malloc(unsigned long size);
void free(void *p);
/* An address inside an instance is not that instance's base, and free takes a base. */
int main(void) {
    int *p = malloc(64);
    free(p + 4);
    return 0;
}
