/* row: 3.5 variable length arrays */
/* allow */
/* blocked: #291 */
/* Section 3.5 says a dynamic extent is not special, because the plane stores run time values
   anyway. The point of the case is that the length is not a constant and everything still has to
   work out, including the last element. It does not get that far yet: a variable length array
   needs a frame that can grow, which is #291, so the compiler refuses it before the monitor sees
   anything. The expectation is written down here so the day the frame lands, the case runs. */
int walk(int count) {
    int values[count];
    int sum = 0;
    int i;
    for (i = 0; i < count; i++) {
        values[i] = i;
    }
    for (i = 0; i < count; i++) {
        sum += values[i];
    }
    return sum;
}

int main(void) {
    return walk(16) == 120 ? 0 : 1;
}
