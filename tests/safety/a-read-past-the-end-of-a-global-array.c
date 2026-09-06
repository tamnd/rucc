/* row: S3 */
/* refuse: J1 */
/* gap: #428 */
/* The same for a static object, which needs the same plane writes done once at start up. */
int small[4];
int next[4];

int main(void) {
    small[0] = 1;
    next[0] = 2;
    return small[4];
}
