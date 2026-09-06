/* row: T4 */
/* refuse: J1 */
/* gap: #428 */
/* The frame is gone and the next call will reuse it, so the write lands in whatever that call
   puts there. Nothing ends an automatic instance's lifetime yet, so nothing notices. */
int *escape(void) {
    int local = 7;
    return &local;
}

int main(void) {
    int *p = escape();
    *p = 1;
    return 0;
}
