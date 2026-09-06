/* row: S8 */
/* refuse: J1 */
void *malloc(unsigned long size);
void free(void *p);
char *strcpy(char *to, const char *from);
char *strcat(char *to, const char *from);
/* The classic strcat, where each string fits and the two of them together do not. Nothing at the
   call site knows the total, because where the second write starts is the first string's own
   terminator, so both numbers are discovered while the call runs. */
int main(void) {
    char *line = malloc(16);
    strcpy(line, "hello ");
    strcat(line, "a tail that takes it well past the end");
    free(line);
    return 0;
}
