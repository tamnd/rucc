/* row: S3 */
/* allow */
/* Read only static data, which lives in a section the loader maps somewhere unrelated to the
   heap. The monitor has to leave everything it does not own alone rather than treat an unknown
   address as suspicious, or every lookup table in the program becomes a report. */
const int primes[8] = {2, 3, 5, 7, 11, 13, 17, 19};

int main(void) {
    const int *cursor = primes;
    int sum = 0;
    int i;
    for (i = 0; i < 8; i++) {
        sum += cursor[i];
    }
    return sum == 77 ? 0 : 1;
}
