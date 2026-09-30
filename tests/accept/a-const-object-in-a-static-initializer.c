/* accept: gnu */
/* reject: iso */
/* message: initializer element is not constant */
/* gcc reads a `const` object of integer type as its value in the initializer of an object with
   static storage, as it does in the size of an array. The strict dialects keep 6.6 as it is
   written. */

const int n = 4;
static const int twice = n * 2;
int nine = twice + 1;

int counted(void)
{
  const int m = 5;
  static int s = m;
  return s;
}
