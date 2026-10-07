/* row: S8 */
/* refuse: J1 */
/* says: in strlen, over its s argument */
unsigned long strlen(const char *s);
/* Four characters and no terminator, in a local this time. The walk is held to the local's own
   size by the capability its caller handed over, and it is refused at the fifth byte rather than
   reading on into whatever is beside it on the stack. */
int main(void) {
    char word[4] = {'a', 'b', 'c', 'd'};
    return (int)strlen(word);
}
