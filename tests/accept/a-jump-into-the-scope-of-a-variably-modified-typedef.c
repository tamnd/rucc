/* reject: all */
/* message: jump into scope of identifier with variably modified type */
/* The length of `T` is worked out where the typedef is, so a jump that lands past it without
   going through it lands somewhere that length was never computed. C11 6.8.6.1p1 says so about
   any identifier with a variably modified type, and a name for the type is one of them. */

int f(int n) {
  goto inside;
  {
    typedef char T[n];
  inside:
    return (int)sizeof(T);
  }
}
