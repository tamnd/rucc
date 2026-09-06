/* row: S2 */
/* refuse: J1 */
/* gap: #431 */
/* The overflow is written through a pointer to a member rather than through the array's name,
   which is how it usually happens: something took the address of a field and kept going. */
struct frame {
    int values[4];
    int flag;
};

int main(void) {
    struct frame frame;
    int *cursor = frame.values;
    int i;
    frame.flag = 0;
    for (i = 0; i < 8; i++) {
        cursor[i] = i;
    }
    return frame.flag;
}
