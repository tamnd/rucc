/* A sweep whose span is much larger than what it reads. */
/* Column major access over a row major array, which is what an image filter reading one channel,
   a structure of arrays walked one field at a time, and the second operand of a matrix multiply
   all look like. The loop touches one element per row and the distance from its first access to
   its last is the whole buffer, so anything the monitor does in front of a loop that scales with
   the span rather than with the number of iterations shows up here and in nothing else in this
   set. That is tamnd/rucc#861, where the extent query a split loop asks walked the lifetime plane
   one granule at a time and the guard cost more than the loop it was guarding. */
void *malloc(unsigned long size);
void free(void *p);

#define ROWS 512
#define COLS 128
#define ROUNDS 200

int main(void) {
    long *grid = malloc((unsigned long)ROWS * COLS * sizeof(long));
    long total = 0;
    int round;
    int row;
    int col;
    for (row = 0; row < ROWS; row++) {
        for (col = 0; col < COLS; col++) {
            grid[row * COLS + col] = row + col;
        }
    }
    for (round = 0; round < ROUNDS; round++) {
        for (col = 0; col < COLS; col++) {
            for (row = 0; row < ROWS; row++) {
                total += grid[row * COLS + col];
            }
        }
    }
    free(grid);
    return total == 0 ? 1 : 0;
}
