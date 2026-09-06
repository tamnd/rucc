/* row: S8 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
void *memcpy(void *to, const void *from, unsigned long count);
unsigned long strlen(const char *s);
/* The other half. Every program in the world calls strlen on a string that is terminated, and a
   boundary check that had anything to say about one would be the check people turn off. */
int main(void) {
    char *text = malloc(16);
    memcpy(text, "hello", 6);
    if (strlen(text) != 5) {
        return 1;
    }
    free(text);
    return 0;
}
