/* row: S6 */
/* refuse: J1 */
/* gap: #431 */
/* The hardware catches this one, which is not the same as the monitor catching it: a report
   would say which access it was and where. That needs a capability whose provenance is nothing,
   which is what S2 builds. */
int main(void) {
    int *p = 0;
    return *p;
}
