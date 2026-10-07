/* row: S8 */
/* allow */
void *memcpy(void *to, const void *from, unsigned long count);
void *memset(void *to, int byte, unsigned long count);
char *strcpy(char *to, const char *from);
char *strcat(char *to, const char *from);
unsigned long strlen(const char *s);
int strcmp(const char *a, const char *b);
struct pair {
    int key;
    char name[12];
};
/* Every one of these hands the wrapper the capability of a local, and every one of them fits in
   it, including the copy into the middle of a buffer and the copy into a member of a struct. A
   member is held to the whole struct unless -fsafety-subobject asks for more, so none of these may
   be refused. */
int main(void) {
    char buf[16];
    char half[8];
    struct pair one;
    struct pair two;
    memset(buf, 0, sizeof buf);
    strcpy(buf, "abc");
    strcat(buf, "defghijklmno");
    memset(half, 'x', sizeof half);
    memcpy(buf + 8, half, 8);
    one.key = 1;
    strcpy(one.name, "eleven char");
    memcpy(&two, &one, sizeof one);
    memcpy(two.name, buf, 4);
    return (int)strlen(one.name) - 11 + strcmp(one.name, "eleven char") + two.key - 1;
}
