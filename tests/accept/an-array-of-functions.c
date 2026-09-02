/* reject: all */
/* message: declaration of 'table' as array of functions */
/* The same rule from the other side: an array of functions has no size to lay out. */

typedef int fn(void);

fn table[4];
