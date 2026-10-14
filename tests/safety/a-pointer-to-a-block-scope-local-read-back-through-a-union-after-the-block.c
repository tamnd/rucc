/* row: T4 */
/* refuse: J1 */
/* The use after scope above with the pointer stored into a union and read back out of the other
   member, so that the read goes through memory and the capability comes from what the store wrote
   down for the word. The block is shut by then and the frame still runs, which is the one way a
   pointer like this can only be that same local after its lifetime. */
union both {
    void *first;
    void *second;
};
int main(void) {
    void *data = 0;
    union both kept;
    {
        int inner = 7;
        data = &inner;
    }
    kept.first = data;
    int *escaped = kept.second;
    return *escaped == 7 ? 0 : 1;
}
