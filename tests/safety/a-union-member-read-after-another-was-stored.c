/* row: 3.5 type punning through a union */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Section 3.5 is specific about this: C has permitted it since C99 TC3 for a union whose address
   has been taken, so Y3 implements 6.5.2.3 and not the folklore version of it. A checker that
   refuses this refuses most of the C written before anybody wrote type punning helpers. */
union bits {
    int as_int;
    char as_bytes[4];
};

int main(void) {
    union bits *u = malloc(sizeof(union bits));
    int low;
    u->as_int = 0x01020304;
    low = u->as_bytes[0];
    free(u);
    return low == 4 ? 0 : 1;
}
