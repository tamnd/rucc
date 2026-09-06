/* row: S1 */
/* refuse: J1 */
/* gap: #428 */
void *malloc(unsigned long size);
void free(void *p);
/* Seventeen bytes are served out of a whole number of granules, so the plane says the bytes past
   the request belong to the instance too. The exact extent is in the instance header, and reading
   a header on every access is what the capability exists to avoid. */
int main(void) {
    char *p = malloc(17);
    p[20] = 1;
    return 0;
}
