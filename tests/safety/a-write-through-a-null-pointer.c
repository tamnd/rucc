/* row: S6 */
/* refuse: J1 */
/* gap: #431 */
/* The hardware catches this one and the monitor should get there first, because a report that
   names the judgement is worth more than a segmentation fault that names nothing. The exit
   status is not part of the expectation here for exactly that reason. */
int main(void) {
    int *p = 0;
    *p = 1;
    return 0;
}
