/* row: 3.5 container_of */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* The kernel's list idiom, in miniature. A pointer to a member is walked back to the object it
   is a member of, which stays inside the same instance. */
struct box {
    int first;
    int second;
};

int main(void) {
    struct box *b = malloc(sizeof(struct box));
    int *member = &b->second;
    struct box *back = (struct box *)((char *)member - sizeof(int));
    back->first = 4;
    free(b);
    return back == b ? 0 : 1;
}
