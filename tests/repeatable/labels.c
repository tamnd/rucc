// A function whose labels are inside a loop, which is the shape that caught the compiler giving a
// different answer in every run.
//
// The front end makes a block for each label the first time something names it and says the block
// has all the predecessors it is going to have once the whole body has been walked. Which order it
// says that in decides the order the values those blocks carry are numbered in, and until
// tamnd/rucc#1396 the order came out of a hash table, so it was a different order in every run of
// the compiler.
//
// The three things that have to be here for it to show are all here. The labels are inside a loop,
// so the block a label starts has predecessors that are themselves waiting on the rest of the
// walk. They jump to each other, so working out what one of them carries asks what another one
// carries. And the variables are live across all of it, so there is something to carry.
//
// Nothing about the answer matters. What `xtask repeatable` reads is the IR, and what it asks is
// whether eight runs of the compiler over this file wrote the same IR eight times.

int labels(int x, int n) {
  int a = x, b = x + 1, c = x + 2;
  for (;;) {
    switch (x) {
      case 0: goto first;
      case 1: goto second;
      case 2: goto third;
      case 3: goto fourth;
    }
    goto done;
  first:
    a += b;
    b += c;
    x = a & 15;
    if (a > n) goto third;
    if (b > n) continue;
    if (c > n) goto done;
  second:
    a += c;
    c += a;
    x = b & 15;
    if (a > n) goto fourth;
    if (b > n) continue;
    if (c > n) goto done;
  third:
    b += a;
    c += b;
    x = c & 15;
    if (a > n) goto first;
    if (b > n) continue;
    if (c > n) goto done;
  fourth:
    c += a;
    a += b;
    x = a & 7;
    if (a > n) goto second;
    if (b > n) continue;
    if (c > n) goto done;
  }
done:
  return a + b + c;
}

int main(void) {
  return labels(2, 40) == 0;
}
