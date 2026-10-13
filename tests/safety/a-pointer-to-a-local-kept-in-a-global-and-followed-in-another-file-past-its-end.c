/* row: S2 */
/* refuse: J1 */
/* with: spend */
extern char *kept;
void spend(int count);
/* The same with the pointer kept in a global, which has no aux either. The function in the other
   file finds the pointer where the global is and its capability in the table under the same
   address. */
int main(void) {
    char buffer[10];
    kept = buffer;
    spend(11);
    return buffer[0];
}
