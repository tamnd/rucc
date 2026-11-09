/* accept: all */
/* One unnamed parameter of type void is a list with no parameters, 6.7.6.3p10, also when a
   typedef name says void. FreeBSD's hptmv driver declares its functions that way. A call with
   no arguments must compile, and so must a definition. */

typedef void MV_VOID;

MV_VOID enable(MV_VOID);

static int count;

MV_VOID enable(MV_VOID) {
  count++;
}

int main(MV_VOID) {
  enable();
  return count - 1;
}
