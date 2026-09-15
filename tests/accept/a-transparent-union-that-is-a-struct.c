/* accept: all */
/* warns: 'transparent_union' attribute ignored */
/* Only a union has members that are alternatives to each other, so only a union can be passed the
   way one of them is. gcc drops the attribute on anything else with a warning, and a structure
   written with it goes on being an ordinary structure. */

struct plain {
  int x;
} __attribute__((transparent_union));

int takes(struct plain v);

int passes(void) {
  struct plain v;
  v.x = 1;
  return takes(v);
}
