/* row: S9 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
void *memset(void *to, int byte, unsigned long count);
long write(int fd, const void *from, unsigned long count);
/* The same call with the count the buffer really has. One bounds comparison against a syscall is
   not a cost anybody can measure, and refusing this would be refusing every program that writes
   anything. */
int main(void) {
    char *line = malloc(16);
    memset(line, 'x', 15);
    line[15] = '\n';
    if (write(1, line, 16) != 16) {
        return 1;
    }
    free(line);
    return 0;
}
