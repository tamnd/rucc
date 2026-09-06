/* row: T4 */
/* refuse: J1 */
/* gap: #428 */
/* Use after scope rather than use after return, which is the harder half of T4 because the frame
   is still there and the storage has only been handed to some other declaration. */
int main(void) {
    int *escaped;
    {
        int inner = 7;
        escaped = &inner;
    }
    {
        int other = 9;
        if (other != 9) {
            return 1;
        }
    }
    return *escaped == 7 ? 0 : 1;
}
