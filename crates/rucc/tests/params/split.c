extern int seen(int);
int sum(int *a, int n)
{
  int s = 0;
  for (int i = 0; i < n; i++) {
    if (seen(a[i]))
      break;
    s += a[i];
  }
  return s;
}
int big(int n)
{
  int a[64];
  for (int i = 0; i < 64; i++)
    a[i] = seen(i);
  int s = 0;
  for (int i = 0; i < n; i++) {
    if (a[i] < 0)
      break;
    s += a[i] * 3;
    s ^= a[i] >> 1;
  }
  return s;
}
