static inline int sm(int a, int b, int c)
{
  int s = a * b + c;
  s ^= s >> 3;
  s += (a | 1) * (b & 7);
  s -= c << 2;
  s ^= (s >> 5) + a;
  s += b * c;
  s ^= s << 1;
  return s + (a ^ c);
}
int usesm(int x, int y, int z) { return sm(x, y, z) + sm(y, z, x); }
