/* reject: all */
/* message: invalid use of undefined type 'struct rq' */
/* Reading the object needs its size, which nothing knows until the structure is defined. */

struct rq;
extern struct rq *p;
void take(struct rq);

void pass(void)
{
  take(*p);
}
