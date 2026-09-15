// `__attribute__((transparent_union))` on a union says that passing the union and passing its
// first member are the same thing, which buys two rules. A parameter of the union type is
// compatible with a parameter of any member's type, so a program may declare the function either
// way round. And a value passed to such a parameter goes into whichever member it fits, so a call
// writes the member's type at the call site and nothing about the union.
//
// glibc's socket headers are what every program meets this through. `bind`, `connect`, `accept`
// and four others take a union of eleven socket address pointers, and a program that passes a
// `struct sockaddr_in *` to one of them is relying on this and on nothing else.

struct one {
  int x;
};
struct two {
  long y;
};

typedef union {
  struct one *first;
  struct two *second;
  void *any;
} __attribute__((__transparent_union__)) arg;

// The member is found by type, wherever it is in the list. `a` goes into `first` and `b` into
// `second`, and neither call says so.
int takes(arg v);

int passes_each_member(struct one *a, struct two *b) { return takes(a) + takes(b); }

// A member no value has the type of takes it when it is a pointer that would be assigned to
// without a word said, which is what makes the `void *` member the catch-all it looks like. A null
// pointer constant is taken by the first pointer member instead, since it would assign to that one
// too and the search stops at the first member that takes the value.
int passes_what_only_the_last_member_takes(char *c) { return takes(c) + takes(0); }

// The union itself is still a union, and a value of it is passed the way any other union value is.
int passes_the_union_itself(arg v) { return takes(v); }

// Declaring the function with a member's type declares the same function, which is the rule that
// lets a program assign one to a pointer written either way. gnulib's signature checks are built
// on exactly this and are what found the attribute missing.
int also_takes(arg v);
int also_takes(struct one *p);

int (*as_the_member)(struct one *) = also_takes;
int (*as_the_union)(arg) = also_takes;

// The attribute after the closing brace is the other place it is written, and the two are the same
// declaration. This union is of two pointers to functions, to say that the rule is about the size
// and the alignment of the members rather than about their being pointers to data.
typedef void (*plain)(void);
typedef void (*with_an_argument)(int);

typedef union {
  plain bare;
  with_an_argument taking;
} __attribute__((transparent_union)) handler;

void install(handler h);

void nothing(void);
void something(int n);

void installs_either(void) {
  install(nothing);
  install(something);
}
