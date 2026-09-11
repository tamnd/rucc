// Which `restrict` scope an access is in, which is the clique and the base on every access and
// is what layer 5 of the alias analysis reads. A clique is one scope that declares `restrict`
// pointers and a base is one of the pointers it declares, so same clique and different base is
// the promise that the two accesses cannot touch the same byte.

// Two pointers in one scope, which is the shape every numeric kernel and every `mem` function
// has. The two accesses in the loop carry one clique and two bases.
void copy(int *restrict to, const int *restrict from, unsigned long n) {
    unsigned long i;
    for (i = 0; i < n; i++) {
        to[i] = from[i];
    }
}

struct pair {
    int a;
    int b;
};

// A member of something reached through a `restrict` pointer is reached through it too, and so
// is a member of something one step along from it. The clique is not the one above, because the
// numbers come from a counter the whole module shares rather than from one per function: an
// inliner that merged two functions which had both numbered their own parameters one would end
// up with four pointers promising things about each other that nobody promised.
void swap(struct pair *restrict p, struct pair *restrict q) {
    p->a = q->b;
    p->b = (q + 1)->a;
}

// The array spelling, where the qualifier is written inside the brackets and the adjustment to a
// pointer is what carries it out. Same promise, and the same two numbers.
void scale(int a[restrict], const int b[restrict], unsigned long n) {
    unsigned long i;
    for (i = 0; i < n; i++) {
        a[i] = a[i] * b[i];
    }
}

// A pointer read out of memory, which is where the walk stops. `q` is a `restrict` pointer and
// `*q` is a pointer that came from somewhere the names do not say, so the access through it
// carries nothing and the read of `q` itself carries `q`.
void indirect(int **restrict q, int *restrict r) {
    **q = *r;
}

// Nothing here is `restrict`, so nothing carries a clique and the scope counter is not spent.
void plain(int *to, const int *from) {
    *to = *from;
}
