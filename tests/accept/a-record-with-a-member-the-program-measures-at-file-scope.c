/* reject: all */
/* message: variably modified 'a' at file scope */
/* Nothing runs at file scope for the length to be worked out in, and the record would have to be
   a different size in every file that included the header, so the member is one only a function
   may declare. */

int n;

struct S {
  int a[n];
};
