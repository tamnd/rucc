/* The other half of the cases that hand a structure holding one pointer to a function in another
   file by value. It writes `count` bytes through the pointer, and knows nothing about where the
   pointer came from but what the frame says. */
struct holder {
    char *data;
};
void carry(struct holder held, int count) {
    char *data = held.data;
    for (int i = 0; i < count; i++)
        data[i] = 'x';
}
