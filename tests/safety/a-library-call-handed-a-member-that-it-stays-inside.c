/* row: S8 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
void *memcpy(void *dst, const void *src, unsigned long n);
void *memset(void *dst, int byte, unsigned long n);
char *strcpy(char *dst, const char *src);
unsigned long strlen(const char *s);
/* The good half of the two rows above, and the idioms that hold a call to more than one member on
   purpose. A copy that fits the member is fine, a member that is not an array is the start of a
   run of members a program clears in one call, and an array at the end of a structure is one a
   program allocates past. None of them may be refused. */
struct pair {
    char first[16];
    void *second;
    void *third;
};

struct line {
    long count;
    long total;
    char text[1];
};

int main(void) {
    struct pair p;
    struct line *l;
    memcpy(p.first, "0123456789abcde", sizeof(p.first));
    memset(&p.second, 0, sizeof(p.second) + sizeof(p.third));
    l = malloc(sizeof(*l) + 32);
    if (l == 0) {
        return 1;
    }
    memset(&l->count, 0, 2 * sizeof(long));
    strcpy(l->text, "longer than one byte");
    if (strlen(l->text) != 20 || p.second != 0 || p.first[15] != '\0') {
        return 1;
    }
    free(l);
    return 0;
}
