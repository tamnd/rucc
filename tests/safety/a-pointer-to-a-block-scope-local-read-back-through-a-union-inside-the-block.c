/* row: T4 */
/* allow */
/* The case above with the read inside the block, where the local is still there. */
union both {
    void *first;
    void *second;
};
int main(void) {
    union both kept;
    {
        int inner = 7;
        kept.first = &inner;
        int *escaped = kept.second;
        if (*escaped != 7) {
            return 1;
        }
    }
    return 0;
}
