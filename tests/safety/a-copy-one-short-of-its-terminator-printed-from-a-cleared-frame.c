/* row: S2 */
/* refuse: J1 */
/* says: in printf_string, over its string argument */
int printf(const char *format, ...);
void *memcpy(void *dst, const void *src, unsigned long n);
void *memset(void *dst, int byte, unsigned long n);
/* Juliet's CWE-170 cases: a copy into a buffer that stops one byte short, so the buffer's last byte
   is never written and the string in it has no terminator. The frame before left zeros on those
   bytes, which is what `clear` is here for, and a zero is where `%s` stops. The buffer is filled
   with a pattern on the way in, so the walk runs to its end and is refused there. */
static void clear(void) {
    char used[512];
    memset(used, 0, sizeof(used));
    printf("%s", used);
}

static void copy(void) {
    char data[150];
    char dest[100];
    memset(data, 'A', sizeof(data) - 1);
    data[sizeof(data) - 1] = '\0';
    memcpy(dest, data, sizeof(dest) - 1);
    printf("%s\n", dest);
}

int main(void) {
    clear();
    copy();
    return 0;
}
