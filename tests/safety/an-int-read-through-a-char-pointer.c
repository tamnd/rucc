/* row: 3.5 type punning through a char pointer */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* The character type exemption of 6.5, which is not a hole in the aliasing rules but part of
   them. Serialization, hashing and byte order swapping all live here and none of them is UB. */
int main(void) {
    int *value = malloc(sizeof(int));
    char *bytes = (char *)value;
    int sum = 0;
    unsigned long i;
    *value = 0x01020304;
    for (i = 0; i < sizeof(int); i++) {
        sum += bytes[i];
    }
    free(value);
    return sum == 10 ? 0 : 1;
}
