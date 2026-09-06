/* row: S3 */
/* allow */
/* A read of read only static storage, which every program does and none of which is the heap. */
int main(void) {
    const char *s = "hello";
    int n = 0;
    while (s[n]) {
        n++;
    }
    return n - 5;
}
