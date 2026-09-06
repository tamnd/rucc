/* row: T3 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* A no-op since C89, and a program that relies on it is not doing anything wrong. */
int main(void) {
    free(0);
    return 0;
}
