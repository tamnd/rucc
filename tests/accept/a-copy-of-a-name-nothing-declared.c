/* reject: all */
/* message: 'nope' undeclared here */
/* `copy` names the declaration whose attributes are taken, and gcc refuses a name it cannot find
   in scope the way it refuses one in an expression. */

__attribute__((copy(nope))) int a;
