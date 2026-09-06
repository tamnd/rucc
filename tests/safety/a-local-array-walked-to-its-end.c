/* row: S2 */
/* allow */
/* An automatic object is not in the heap the monitor watches, and saying anything about one
   would be a false positive rather than a finding. */
int main(void) {
    int local[16];
    int i;
    int sum = 0;
    for (i = 0; i < 16; i++) {
        local[i] = i;
    }
    for (i = 0; i < 16; i++) {
        sum += local[i];
    }
    return sum - 120;
}
