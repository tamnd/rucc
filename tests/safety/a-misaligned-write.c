/* row: S7 */
/* refuse: J1 */
/* gap: #428 */
void *malloc(unsigned long size);
void free(void *p);
/* x86 will do this without complaining, which is why it survives in code that is then ported to
   something that will not. Alignment is a plane check and no plane checks it yet. */
int main(void) {
    char *bytes = malloc(64);
    int *misaligned = (int *)(bytes + 1);
    *misaligned = 7;
    free(bytes);
    return 0;
}
