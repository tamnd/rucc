/* row: T1 */
/* refuse: J1 */
/* says: which has been freed */
void *malloc(unsigned long size);
void free(void *p);
/* The free loop that reads `next` after releasing the node it lives in, which is the bug the
   correct version of this loop exists to avoid and the one people write first. */
struct node {
    struct node *next;
    int value;
};

int main(void) {
    struct node *head = 0;
    int i;
    for (i = 0; i < 4; i++) {
        struct node *fresh = malloc(sizeof(struct node));
        fresh->next = head;
        head = fresh;
    }
    while (head) {
        free(head);
        head = head->next;
    }
    return 0;
}
