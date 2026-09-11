/* row: 3.5 variable length arrays */
/* allow */
/* Section 3.5 says a dynamic extent is not special, because the plane stores run time values
   anyway. The point of the case is that the length is not a constant and everything still has to
   work out, including the last element. It was blocked on #291 until the frame learned to grow,
   which is what it was waiting for, and it runs now. */
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
