/* row: S3 */
/* refuse: J1 */
/* Static storage is one object per variable that lives for the whole run, and the compiler knows
   its size where it takes its address, so the capability it builds there is exact. */
int table[16];

int main(void) {
    table[16] = 1;
    return table[0];
}
