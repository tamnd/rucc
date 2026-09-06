/* row: S1 */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Recursion, a hundred and twenty seven live instances, and a free order that is not the order
   they were allocated in. Nothing here is a bug and all of it has to stay quiet. */
struct tree {
    struct tree *left;
    struct tree *right;
    int depth;
};

struct tree *grow(int depth) {
    struct tree *node = malloc(sizeof(struct tree));
    node->depth = depth;
    if (depth > 0) {
        node->left = grow(depth - 1);
        node->right = grow(depth - 1);
    } else {
        node->left = 0;
        node->right = 0;
    }
    return node;
}

int count(struct tree *node) {
    int total;
    if (!node) {
        return 0;
    }
    total = 1 + count(node->left) + count(node->right);
    free(node);
    return total;
}

int main(void) {
    return count(grow(6)) == 127 ? 0 : 1;
}
