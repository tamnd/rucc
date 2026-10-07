/* row: S2 */
/* allow */
char *strcpy(char *to, const char *from);
void *memcpy(void *to, const void *from, unsigned long count);
/* The two shapes before this one with copies that fit: a pointer set on one side of a branch and
   left unset on the other, and one set to one of two buffers of different sizes. Each carries the
   bounds of the buffer it is, so nothing here is refused. */
int main(int argc, char **argv) {
    char *data;
    char *other;
    char buffer[11];
    char small[4];
    char large[16];
    char source[11] = "AAAAAAAAAA";
    (void)argv;
    if (argc > 0) {
        data = buffer;
        data[0] = 0;
    }
    strcpy(data, source);
    if (argc > 5)
        other = small;
    else
        other = large;
    memcpy(other, source, argc > 5 ? 4 : 11);
    return data[10] + other[10];
}
