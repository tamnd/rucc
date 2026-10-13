/* row: S2 */
/* refuse: J1 */
/* with: follow */
void follow(char **where, int count);
/* The buffer's address goes into a pointer variable and the variable's address goes to a function
   in another file, which reads the pointer back out of memory and writes one byte too many through
   it. A word on the stack has no aux beside it, so the capability is written into the thread's own
   table when the pointer is stored and found there when it is loaded. */
int main(void) {
    char buffer[10];
    char *data = buffer;
    follow(&data, 11);
    return buffer[0];
}
