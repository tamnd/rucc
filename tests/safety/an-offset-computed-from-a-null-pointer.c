/* row: 3.5 offsetof on a null pointer */
/* allow */
/* The hand rolled offsetof that every codebase had before stddef.h was reliable. No access ever
   happens, so nothing about it is a violation, and section 3.5 asks for the frontend to fold it
   rather than for the runtime to see a derivation from a null pointer. */
struct record {
    int first;
    int second;
    int third;
};

int main(void) {
    unsigned long offset = (unsigned long)&((struct record *)0)->third;
    return offset == 8 ? 0 : 1;
}
