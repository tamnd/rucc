/* row: S7 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *p);
/* A wire format read in place, which is where misalignment actually comes from. The offset is a
   whole number of fields and none of the fields were eight bytes wide. */
int main(void) {
    char *packet = malloc(64);
    long *field;
    long seen;
    int i;
    /* The packet arrives filled, so that the only thing wrong with the read below is its
       alignment. */
    for (i = 0; i < 64; i++) {
        packet[i] = 0;
    }
    field = (long *)(packet + 4);
    seen = *field;
    free(packet);
    return seen == 0 ? 0 : 1;
}
