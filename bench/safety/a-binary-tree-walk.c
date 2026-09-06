/* Document 12 section 12.4's second addition. */
/* Same two lines per node as the list, with a branch at every step and no locality at all past
   the top few levels. The recursion also means a call on every node, which document 08 section
   8.8 says kills every liveness fact the compiler had, so this is where a check that could have
   been eliminated in a loop cannot be. */
void *malloc(unsigned long size);
void free(void *p);

struct tree {
    struct tree *left;
    struct tree *right;
    long value;
};

struct tree *grow(int depth) {
    struct tree *node = malloc(sizeof(struct tree));
    node->value = depth;
    if (depth > 0) {
        node->left = grow(depth - 1);
        node->right = grow(depth - 1);
    } else {
        node->left = 0;
        node->right = 0;
    }
    return node;
}

long walk(struct tree *node) {
    if (!node) {
        return 0;
    }
    return node->value + walk(node->left) + walk(node->right);
}

void burn(struct tree *node) {
    if (!node) {
        return;
    }
    burn(node->left);
    burn(node->right);
    free(node);
}

int main(void) {
    struct tree *root = grow(18);
    long sum = 0;
    int round;
    for (round = 0; round < 8; round++) {
        sum += walk(root);
    }
    burn(root);
    return sum == 0 ? 1 : 0;
}
