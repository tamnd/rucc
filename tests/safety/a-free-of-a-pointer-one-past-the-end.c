/* row: T3 */
/* refuse: J6 */
void *malloc(unsigned long size);
void free(void *p);
/* The cursor was freed instead of the buffer, after the loop had walked it to the end. It is one
   past a real allocation, which is a legal address and not an allocation base. */
int main(void) {
    char *p = malloc(16);
    char *cursor = p;
    int i;
    for (i = 0; i < 16; i++) {
        cursor++;
    }
    free(cursor);
    return 0;
}
