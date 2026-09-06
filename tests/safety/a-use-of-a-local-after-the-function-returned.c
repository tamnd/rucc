/* row: T4 */
/* refuse: J1 */
/* gap: #428 */
/* The frame is gone and nothing said so, because an automatic instance has no plane entry until
   S2 writes one at the scope boundaries. */
static int *escape(void) {
    int local[16];
    local[0] = 3;
    return local;
}

int main(void) {
    int *p = escape();
    return p[0];
}
