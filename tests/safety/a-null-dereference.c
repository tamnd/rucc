/* row: S6 */
/* refuse: J1 */
/* The hardware catches this one, which is not the same as the monitor catching it: a report
   says which access it was and where. The pointer's capability is the null one, whose
   provenance is nothing, and the bounds check refuses it before the load faults. */
int main(void) {
    int *p = 0;
    return *p;
}
