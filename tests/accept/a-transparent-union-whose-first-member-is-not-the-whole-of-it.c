/* accept: all */
/* warns: 'transparent_union' attribute ignored */
/* What the attribute promises is that passing the union and passing its first member are the same
   thing, and a union wider than its first member is one where that cannot be arranged. gcc asks
   the same question and drops the attribute the same way, with a warning rather than an error,
   because the union is still a perfectly good union and the program still compiles. */

union both {
  int small;
  double large;
} __attribute__((transparent_union));

int takes(union both v);

int passes(void) {
  union both v;
  v.small = 1;
  return takes(v);
}
