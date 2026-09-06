/* row: S8 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *p);
char *strcpy(char *to, const char *from);
char *strncat(char *to, const char *from, unsigned long count);
/* The misreading strncat invites. Its count says how much of the source to take and says nothing
   at all about how much room the destination has, so a count that looks safe next to the buffer
   size is still an overflow once what is already there is counted. */
int main(void) {
    char *line = malloc(16);
    strcpy(line, "hello ");
    strncat(line, "a source that is longer than this", 16);
    free(line);
    return 0;
}
