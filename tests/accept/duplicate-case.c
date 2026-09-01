/* reject: all */
/* message: duplicate case value */
/* Two labels on one value, which no dialect has ever allowed. Here to keep the suite honest: a
   harness that only ever runs programs that compile is a harness that would pass if the
   compiler accepted everything. */

int which(int n) {
  switch (n) {
    case 1:
      return 1;
    case 1:
      return 2;
  }
  return 0;
}
