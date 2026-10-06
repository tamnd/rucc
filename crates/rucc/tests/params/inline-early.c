static inline int sq(int a, int b) { return a * b + (a ^ b) - (b << 2) + (a | b) * 3; }
__attribute__((cold)) int c(int x, int y) { return sq(x, y) + sq(y, x) + sq(x, x); }
int h(int x) { return sq(x, x + 1); }
