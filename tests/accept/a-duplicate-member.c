/* reject: all */
/* message: duplicate member 'a' */
/* Two members of one struct cannot share a name, and the message names the one that was
   written twice rather than the struct. */

struct s { int a; int a; };
