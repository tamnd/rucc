/* row: S1 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* The list is freed from the front, which means each node is read for its next pointer and then
   released, and the read has to happen before the release and be seen to. */
struct node {
    struct node *next;
    int value;
};

int main(void) {
    struct node *head = 0;
    int sum = 0;
    int i;
    for (i = 0; i < 8; i++) {
        struct node *fresh = malloc(sizeof(struct node));
        fresh->next = head;
        fresh->value = i;
        head = fresh;
    }
    while (head) {
        struct node *next = head->next;
        sum += head->value;
        free(head);
        head = next;
    }
    return sum == 28 ? 0 : 1;
}
