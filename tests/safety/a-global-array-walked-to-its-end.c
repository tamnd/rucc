/* row: S3 */
/* allow */
/* The same for a static object. */
int global[16];

int main(void) {
    int i;
    int sum = 0;
    for (i = 0; i < 16; i++) {
        global[i] = i;
    }
    for (i = 0; i < 16; i++) {
        sum += global[i];
    }
    return sum - 120;
}
