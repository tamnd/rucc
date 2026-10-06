static int mix(const int *a, int n, int k)
{
  int s = 0;
  for (int i = 0; i < n; i++) {
    int v = a[i] * k;
    s += (v ^ (v >> 3)) + (v & 7) * (a[i] | 1);
    if (s > 1000) s -= a[i] >> 2;
    s ^= (s >> 7) + i;
  }
  return s;
}
int big(const int *a)
{
  int s = 0;
  s += mix(a, 2, 1); s += mix(a, 3, 2); s += mix(a, 4, 3); s += mix(a, 5, 4);
  s += mix(a, 6, 5); s += mix(a, 7, 6); s += mix(a, 8, 7); s += mix(a, 9, 8);
  return s;
}
