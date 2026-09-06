/* row: S8 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
unsigned long strlen(const char *s);
char *strcpy(char *to, const char *from);
/* The other half of the row above, and the one that matters more. Almost every strcpy in almost
   every program is this, and a monitor that could not tell it from the overrun would be a monitor
   nobody could turn on. */
int main(void) {
    char *to = malloc(16);
    strcpy(to, "hello");
    if (strlen(to) != 5) {
        return 1;
    }
    free(to);
    return 0;
}
