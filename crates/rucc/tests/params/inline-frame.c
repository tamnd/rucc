extern void fill(char *, int);
static int big(int n)
{
  char buf[4000];
  fill(buf, n);
  return buf[n & 63];
}
int top(int n)
{
  char mine[512];
  fill(mine, n);
  return big(n) + mine[n & 7];
}
