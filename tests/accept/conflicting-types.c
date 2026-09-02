/* reject: all */
/* message: conflicting types for 'g'; have 'char(int)' */
/* The second declaration is checked against the first and the message spells the type it
   arrived with, hard against the parameter list the way gcc writes it. */

int g(int);

char g(int);
