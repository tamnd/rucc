/* The other half of the cases that keep a pointer to a local in a word on the stack. It reads the
   pointer out of the word it was given and writes `count` bytes through it. */
void follow(char **where, int count) {
    char *to = *where;
    for (int i = 0; i < count; i++)
        to[i] = 'x';
}
