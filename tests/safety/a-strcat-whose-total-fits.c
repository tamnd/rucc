/* row: S8 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
unsigned long strlen(const char *s);
char *strcpy(char *to, const char *from);
char *strcat(char *to, const char *from);
/* Building a string in a buffer that was sized for it, which is what the same three lines look
   like when the program got them right. */
int main(void) {
    char *line = malloc(32);
    strcpy(line, "hello ");
    strcat(line, "world");
    if (strlen(line) != 11) {
        return 1;
    }
    free(line);
    return 0;
}
