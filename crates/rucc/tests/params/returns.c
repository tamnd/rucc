extern int work(int);
extern int more(int);
int neg(int x)
{
  if (x > 10)
    return -1;
  int s = work(x);
  s += more(s);
  return s * 3;
}
void *nul(void *p, int x)
{
  if (x > 10)
    return 0;
  work(x);
  more(x);
  return p;
}
int cont(int n)
{
  int s = 0;
  for (int i = 0; i < n; i++) {
    if (work(i) > 3)
      continue;
    s += more(i);
    s ^= work(s);
  }
  return s;
}
int far(int x)
{
  if (x > 10) {
    work(1);
    more(2);
    work(3);
    more(4);
    work(5);
    return -1;
  }
  int s = work(x);
  s += more(s);
  return s * 3;
}
