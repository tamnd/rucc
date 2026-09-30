/* accept: all */
/* `*p` on a structure that has no definition yet is an lvalue, and taking its address or its type
   does not read it. The kernel's `PERCPU_PTR(&runqueues)` does both on a `struct rq` that
   `sched.h` only declares, and gcc accepts it. */

struct rq;
extern struct rq runqueues;

struct rq *here(void)
{
  return &*(&runqueues);
}

unsigned long pinned(void)
{
  return (unsigned long)(__typeof__(*(&runqueues)) *)(unsigned long)(&runqueues) + 8;
}
