extern void fill(char *, int);
static int small(int n)
{
  char buf[64];
  fill(buf, n);
  return buf[n & 63];
}
int top(int n) { return small(n) + 1; }
