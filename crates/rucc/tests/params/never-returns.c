extern void die(int) __attribute__((noreturn));
extern void warm(int) __attribute__((cold));
extern int work(int);
int f(int x, int y)
{
  int s = 0;
  for (int i = 0; i < x; i++) {
    if (i == y) die(i);
    s += work(i);
  }
  return s;
}
int g(int x, int y)
{
  int s = 0;
  for (int i = 0; i < x; i++) {
    if (i == y) warm(i);
    s += work(i);
  }
  return s;
}
