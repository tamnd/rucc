/* row: T9 */
/* flags: -fsafety-leaks */
/* allow */
void *malloc(unsigned long size);
/* Never freed, and not lost either: a global points at a node and the node points at its data, so
   both are reachable at exit. The sweep follows pointers out of the heap as well as into it, and a
   sweep that only looked at the roots would report the second block. */
struct node {
    struct node *next;
    char *data;
};

struct node *head;

int main(void) {
    head = malloc(sizeof(struct node));
    head->next = 0;
    head->data = malloc(100);
    head->data[99] = 7;
    return head->data[99] - 7;
}
