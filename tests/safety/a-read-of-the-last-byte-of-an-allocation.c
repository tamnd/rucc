/* row: S1 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* The byte an off by one gets wrong in the safe direction. */
int main(void) {
    char *p = malloc(17);
    p[16] = 3;
    return p[16] - 3;
}
