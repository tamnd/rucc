/* row: 6.2.2 the slot beside a copied pointer */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* A structure with a pointer in it, assigned whole rather than member by member, which is what the
   front end turns into a copy of the bytes. The pointer moves as bytes and no store ever sees it,
   so the only thing that can carry its capability over to the slot beside its new home is the copy
   itself. Without that the destination slot says nothing, the pointer read back out of it permits
   nothing, and the first access through it is refused on a program that does nothing wrong. This is
   every swap of two structures and every fill of one from another, and libwebp's encoder does it
   three times in a row to exchange two lists of references. tamnd/rucc#1471. */
struct band {
    int *values;
    int count;
    int width;
};

int main(void) {
    struct band *one = malloc(sizeof *one);
    struct band *two = malloc(sizeof *two);
    int *room = malloc(4 * sizeof(int));
    int answer;

    room[0] = 6;
    room[1] = 7;
    two->values = room;
    two->count = 2;
    two->width = 1;

    *one = *two;

    answer = one->values[0] + one->values[1] != 13;
    free(room);
    free(two);
    free(one);
    return answer;
}
