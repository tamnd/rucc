void f(const char *, int, int);
void g(void) {
  { f("select a \
 from t \
 where b", 1, 2);}
  { f("select a \
 from t", 1, 2);}
  { f("x \
y" , 3, 4); }
  { f("x \
y",5,6); }
}
