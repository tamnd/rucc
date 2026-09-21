// Records with a member whose length the program computes, which C calls variably modified and
// which only a function may declare. The layout is the ordinary one and it is worked out where
// the declaration is reached rather than here: the size and the offsets after the first member
// of no fixed size are arithmetic over the sizes of the members in front of them.

int use(void *);

// The size is the members added up and padded, and the member after the one of no fixed size
// sits at a variable offset. There is no rounding in front of `j`, since the end of `i` is a
// multiple of four however long `i` is.
long a_member_and_one_after_it(int n) {
  struct S {
    int i[n];
    int j;
  } s;
  s.j = 1;
  return (long)sizeof s + use(&s);
}

// The members in front of the variable one sit where the numbers say, and only the ones after it
// are worked out. The `double` needs the rounding, because the end of `i` is four-aligned and
// eight is asked for.
long members_on_both_sides(int n) {
  struct S {
    char c;
    int i[n];
    double d;
  } s;
  s.c = 1;
  s.d = 2;
  return (long)sizeof s + use(&s);
}

// Two of them, so the second offset is counted from the end of the first variable member and the
// record is as long as both plus what the alignments asked for.
long two_variable_members(int n) {
  struct S {
    char a[n];
    short h;
    char b[n];
    int j;
  } s;
  s.h = 1;
  s.j = 2;
  return (long)sizeof s + use(&s);
}

// A union is as long as its longest member, which is a comparison rather than a sum.
long a_union(int n) {
  union U {
    int i[n];
    double d;
  } u;
  u.d = 1;
  return (long)sizeof u + use(&u);
}

// `packed` takes the alignment down to one byte, so nothing is rounded and an access through a
// member may not assume the alignment the member's type would otherwise have.
long packed(int n) {
  struct __attribute__((packed)) S {
    char c;
    int i[n];
    int j;
  } s;
  s.j = 1;
  return (long)sizeof s + use(&s);
}

// `aligned` on a member raises where that member lands, which is a rounding the program does.
long an_aligned_member(int n) {
  struct S {
    char c;
    int i[n];
    int __attribute__((aligned(16))) j;
  } s;
  s.j = 1;
  return (long)sizeof s + use(&s);
}

// One inside another, where the inner record is the member of no fixed size.
long nested(int n) {
  struct In {
    int a[n];
    int x;
  };
  struct Out {
    char c;
    struct In in;
    int y;
  } o;
  o.in.x = 1;
  o.y = 2;
  return (long)sizeof o + use(&o);
}

// A step over one of these is a multiplication by a size the program worked out, the same as a
// step over a variable length array is.
long an_array_of_them(int n) {
  struct S {
    int i[n];
    int j;
  } a[2];
  a[1].j = 1;
  return (long)sizeof a + use(a);
}

// The size is fixed where the definition of the record is reached, so a later assignment to the
// length changes nothing. That is C23 6.7.7.3p12, and it is why a record definition with nothing
// declared after it is still a declaration the walk has to meet.
long measured_at_the_definition(int n) {
  struct S {
    int a[n];
  };
  n = 0;
  return (long)sizeof(struct S);
}

// `offsetof` of a member after the variable one is arithmetic for the same reason the size is,
// and so is one whose path steps over an element of no fixed size.
long offsets(int n, int i) {
  struct S {
    int a[n];
    int b;
  };
  typedef int T[n];
  struct R {
    int a;
    T b[n];
  };
  return (long)__builtin_offsetof(struct S, b) + (long)__builtin_offsetof(struct R, b[i]);
}
