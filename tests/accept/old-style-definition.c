/* accept: all */
/* warns: c23 gnu23: old-style function definition */
/* A definition that names its parameters and then declares them, which is the only kind C had
   before C89 added prototypes. C23 removed it from the language and gcc still accepts it in
   every dialect, with a warning under C23, which is what the directives above say. A name with
   no declaration under the list is an `int`, which is silent in C89 and a diagnostic after it,
   so there is none of that here. */

int add(a, b)
int a;
int b;
{
  return a + b;
}

/* The declarations are in whatever order suits, they may declare more than one name at a time,
   and a `register` on one of them is the storage class a parameter is allowed. */
int span(first, last, step)
register int step;
int first, last;
{
  return (last - first) / step;
}

/* A record parameter is passed and returned the same way it is through a prototype. */
struct rect {
  int w;
  int h;
};
int area(box)
struct rect box;
{
  return box.w * box.h;
}

/* A parameter narrower than an `int` keeps its own type inside the body, and what the caller
   hands over is the promoted one. A prototype above the definition is the pairing all the code
   written this way relies on. */
int narrow(char);
int narrow(c)
char c;
{
  return (int)sizeof c;
}

/* An array parameter is a pointer here as much as it is in a prototype. */
int first(a)
int a[4];
{
  return a[0];
}
