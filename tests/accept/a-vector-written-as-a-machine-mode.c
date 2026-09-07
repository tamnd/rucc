/* reject: all */
/* message: vector machine mode */
/* This is the one place `mode` is refused where gcc accepts. gcc builds the vector and warns
   that the spelling is deprecated, pointing at `vector_size` instead, and this compiler does not
   build a vector from a mode yet. Ignoring it would declare one lane where the program asked for
   four, which is a wrong answer rather than a missing feature, so it is refused and the note
   names the spelling that works. */

typedef int __attribute__((mode(V4SI))) v4si;

v4si f(v4si a) { return a; }
