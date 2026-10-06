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
int once0(const int *a, int k) { return mix(a, 4, k); }
int once1(const int *a, int k) { return mix(a, 5, k); }
int once2(const int *a, int k) { return mix(a, 6, k); }
int loop0(const int *a, int m, int k) { int s = 0; for (int j = 0; j < m; j++) s += mix(a + j, 4, k); return s; }
int loop1(const int *a, int m, int k) { int s = 0; for (int j = 0; j < m; j++) s += mix(a + j, 5, k); return s; }
int loop2(const int *a, int m, int k) { int s = 0; for (int j = 0; j < m; j++) s += mix(a + j, 6, k); return s; }
