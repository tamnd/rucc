/* accept: all */
/* warns: 'alloc_size' attribute argument value '2' exceeds the number of function parameters 1 */
/* gcc drops an `alloc_size` that names an argument the function does not have, with a warning,
   and the declaration stands without it. */

__attribute__((alloc_size(2))) void *a(unsigned long);
