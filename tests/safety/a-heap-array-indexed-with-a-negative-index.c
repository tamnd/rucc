/* row: S1 */
/* refuse: J2 */
void *malloc(unsigned long size);
void free(void *p);
/* A signed index that went negative, which is what a subtraction of two unrelated lengths does
   and what a loop counter does when its bound was computed from the wrong array. */
int main(void) {
    int *p = malloc(64);
    int i = 0;
    while (i < 4) {
        i++;
    }
    p[-i] = 7;
    free(p);
    return 0;
}
