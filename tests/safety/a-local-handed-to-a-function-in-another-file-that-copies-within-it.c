/* row: S8 */
/* allow */
/* with: fill */
void *memset(void *to, int byte, unsigned long count);
void fill(char *to, const char *from, unsigned long count);
/* The same two files with a copy that fits. The bounds that cross with the pointer are the bounds
   of `data`, so a copy of exactly its size is left alone. */
int main(void) {
    char data[50];
    char source[100];
    memset(source, 'C', 99);
    source[99] = 0;
    fill(data, source, 50);
    return data[0] == 'C' ? 0 : 1;
}
