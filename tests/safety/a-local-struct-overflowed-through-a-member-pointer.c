/* row: S2 */
/* refuse: J1 */
/* The overflow is written through a pointer to a member rather than through the array's name,
   which is how it usually happens: something took the address of a field and kept going. The
   pointer is derived from the local, so it carries the local's capability, and the write past the
   end of the structure is refused. Running from one member into the next stays inside the object
   and is -fsafety-subobject's to refuse. */
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
