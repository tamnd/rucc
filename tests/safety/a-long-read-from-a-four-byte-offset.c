/* row: S7 */
/* refuse: J1 */
/* gap: #431 */
void *malloc(unsigned long size);
void free(void *p);
/* A wire format read in place, which is where misalignment actually comes from. The offset is a
   whole number of fields and none of the fields were eight bytes wide. */
int main(void) {
    char *packet = malloc(64);
    long *field = (long *)(packet + 4);
    long seen = *field;
    free(packet);
    return seen == 0 ? 0 : 1;
}
