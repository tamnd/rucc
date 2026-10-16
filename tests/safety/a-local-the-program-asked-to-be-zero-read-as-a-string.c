/* row: S2 */
/* flags: -ftrivial-auto-var-init=zero */
/* allow */
unsigned long strlen(const char *s);
/* A buffer that is written three bytes into and then read as a string, which relies on the rest of
   it being zero. Under -ftrivial-auto-var-init=zero the program asked for that, so the pattern a
   build with safety on otherwise fills a local with must not be put over it. */
static void name(char *to) {
    to[0] = 'a';
    to[1] = 'b';
    to[2] = 'c';
}

int main(void) {
    char text[16];
    name(text);
    return strlen(text) == 3 ? 0 : 1;
}
