/* Document 12 section 12.4's first addition, and document 05 section 5.5's worst case. */
/* One cache line per node becomes two once the node's capability has to be read as well, and a
   list walk has no arithmetic to hide the second miss behind. This is the shape the kernel is
   made of, so if any row is going to be embarrassing it is this one. */
void *malloc(unsigned long size);
void free(void *p);

struct node {
    struct node *next;
    long value;
};

int main(void) {
    struct node *head = 0;
    long sum = 0;
    int round;
    int i;
    for (i = 0; i < 200000; i++) {
        struct node *fresh = malloc(sizeof(struct node));
        fresh->next = head;
        fresh->value = i;
        head = fresh;
    }
    for (round = 0; round < 20; round++) {
        struct node *cursor = head;
        while (cursor) {
            sum += cursor->value;
            cursor = cursor->next;
        }
    }
    while (head) {
        struct node *next = head->next;
        free(head);
        head = next;
    }
    return sum == 0 ? 1 : 0;
}
