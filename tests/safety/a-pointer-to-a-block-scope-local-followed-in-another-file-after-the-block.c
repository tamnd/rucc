/* row: T4 */
/* refuse: J1 */
/* with: follow */
/* A pointer to a buffer declared in a block is kept in a local, and the local's address is handed
   to a function in another file once the block has closed. That function reads the pointer out of
   the word it was given, so what it knows of the buffer is what the store wrote down beside it. */
void follow(char **where, int count);
int main(void) {
    char *kept = 0;
    {
        char buffer[10];
        kept = buffer;
    }
    follow(&kept, 1);
    return 0;
}
