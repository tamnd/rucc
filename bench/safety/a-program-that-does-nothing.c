/* The startup charge on its own. */
/* Mapping the planes and setting up the arena happens before main and is paid by every process,
   so a program that does no work at all measures it and nothing else. Section 13.4 rule 5 asks
   for cold start separately from steady state, and this is the cold start: whatever it costs
   here is included in every other row, and the difference between the two builds of this program
   is what a shell script paying it a thousand times would pay. */
int main(void) {
    return 0;
}
