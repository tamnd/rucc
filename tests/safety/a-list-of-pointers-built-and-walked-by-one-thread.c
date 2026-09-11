/* row: C2 */
/* flags: -fsafety-races=pointer */
/* allow */
/* The case the race check has to be silent about, and the one nearly every program is. One thread
   stores pointers and reads them back, so every stamp the epoch plane holds is that thread's own
   and there is nothing to report. It is here to keep the emitted calls honest end to end: the
   checks and the recordings are real calls into the runtime, they have to link, and a program that
   races with nobody has to run to the end and say nothing. `=pointer` rather than `=metadata`
   because it is the mode that asks at a read as well as at a store, so this covers both calls. */
void *malloc(unsigned long size);
void free(void *p);

struct node {
    struct node *next;
    int value;
};

int main(void) {
    struct node *head = 0;
    int i;
    int sum = 0;
    for (i = 0; i < 8; i++) {
        struct node *made = malloc(sizeof(struct node));
        made->next = head;
        made->value = i;
        head = made;
    }
    while (head) {
        struct node *next = head->next;
        sum += head->value;
        free(head);
        head = next;
    }
    return sum == 28 ? 0 : 1;
}
