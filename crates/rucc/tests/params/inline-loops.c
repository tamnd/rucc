static int mix(const int *a, int n, int k)
{
  int s = 0;
  for (int i = 0; i < n; i++) {
    int v = a[i] * k;
    s += (v ^ (v >> 3)) + (v & 7) * (a[i] | 1);
    if (s > 1000) s -= a[i] >> 2;
    if (s < -1000) s += a[i] << 1;
    s ^= (s >> 7) + i;
  }
  return s;
}
int use1(const int *a) { return mix(a, 4, 3); }
int use2(const int *a, int n) { return mix(a, n, 5) + mix(a + 1, n, 6); }
