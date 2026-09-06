/* row: S1 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Row times width plus column, computed by hand, over every cell. The last cell is the one that
   an off by one in the index arithmetic would push out of the object. */
int main(void) {
    int rows = 8;
    int width = 12;
    int *grid = malloc(rows * width * sizeof(int));
    int row;
    int column;
    for (row = 0; row < rows; row++) {
        for (column = 0; column < width; column++) {
            grid[row * width + column] = row + column;
        }
    }
    if (grid[7 * 12 + 11] != 18) {
        return 1;
    }
    free(grid);
    return 0;
}
