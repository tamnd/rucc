/* row: S3 */
/* refuse: J1 */
/* gap: #428 */
/* Static storage is one instance per object that lives for the whole run, so it is the easiest
   of the three to give a capability to and the one nothing writes a plane entry for yet. */
int table[16];

int main(void) {
    table[16] = 1;
    return table[0];
}
