/* accept: all */
/* A zero length array, which gcc allows and real code uses as the tail of a structure. The
   object has an image with nothing in it, which is not the same as an object with no image, and
   the IR reader used to stop on the empty one. */

unsigned char foo[1][0];

char tail[0] = { };

int main(void) { return sizeof foo; }
