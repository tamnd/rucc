int w(int a, int b)
{
  int s = a ^ b;
  s ^= s >> 1;
  s += a * 4;
  s -= b << 3;
  s ^= s >> 4;
  s += a * 7;
  s -= b << 1;
  s ^= s >> 7;
  s += a * 10;
  s -= b << 4;
  s ^= s >> 3;
  s += a * 13;
  s -= b << 2;
  s ^= s >> 6;
  s += a * 16;
  return s;
}
int caller(int x, int y) { return w(x, y) + 1; }
