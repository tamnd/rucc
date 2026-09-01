/* accept: all */
/* gap: #100 all */
/* A definition that names its parameters and then declares them. C23 removed it from the
   language and gcc still accepts it with a warning, which is what "accept" means here. */

int add(a, b)
int a;
int b;
{
  return a + b;
}
