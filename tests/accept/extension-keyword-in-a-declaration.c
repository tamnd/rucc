/* accept: all */
/* `__extension__` says the program means the extension it is about to use, so that
   `-pedantic` says nothing about it. It is written in front of a declaration as often as in
   front of an expression: every declaration in glibc that mentions `long long` begins with
   one, which is why a compiler that stops at it stops at `stdlib.h`. */

__extension__ typedef struct
  {
    long long int quot;
    long long int rem;
  } lldiv_t;

__extension__ extern long long int atoll(const char *nptr);

struct holder
  {
    int plain;
    __extension__ long long int wide;
  };

int use(void)
{
  __extension__ long long int local = 1;
  __extension__ int counted = 2;
  /* The same keyword in front of an expression, which is where it was already taken and
     which is what makes the one in front of a declaration a decision about lookahead. */
  int value = __extension__ (counted + 1);
  return (int)local + value;
}
