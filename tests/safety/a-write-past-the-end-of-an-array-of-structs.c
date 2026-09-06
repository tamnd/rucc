/* row: S1 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *p);
/* Room for four and a fifth one appended, which is the shape of every hand rolled growable
   array that forgot to grow. */
struct point {
    int x;
    int y;
};

int main(void) {
    struct point *points = malloc(4 * sizeof(struct point));
    points[4].x = 1;
    free(points);
    return 0;
}
