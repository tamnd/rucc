/* row: S3 */
/* refuse: J1 */
/* The same for a static object. The read lands in the next global, which is real storage nobody
   would fault on, and the capability made out of the variable's declared size refuses it. */
int small[4];
int next[4];

int main(void) {
    small[0] = 1;
    next[0] = 2;
    return small[4];
}
