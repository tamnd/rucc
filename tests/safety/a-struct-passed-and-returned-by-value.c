/* row: S2 */
/* allow */
/* Passing a struct by value copies it into the callee's frame and returning one copies it back
   out, which means addresses are taken of things the source never named. All of it is ordinary
   and none of it may produce a report. */
struct pair {
    long first;
    long second;
};

struct pair swap(struct pair in) {
    struct pair out;
    out.first = in.second;
    out.second = in.first;
    return out;
}

int main(void) {
    struct pair p;
    struct pair swapped;
    p.first = 3;
    p.second = 4;
    swapped = swap(p);
    return swapped.first == 4 && swapped.second == 3 ? 0 : 1;
}
