/* accept: all */
/* warns: automatic storage duration */
/* A `static` lives until the program ends and a file scope object outlives every block, so there
   is no point on the way out of anything to call the handler at. gcc drops the attribute with a
   warning and so does this: dropping it silently is what makes the leak hard to find. */

void release(int *held);

static int kept __attribute__((cleanup(release)));

void f(void) {
  (void)kept;
}
