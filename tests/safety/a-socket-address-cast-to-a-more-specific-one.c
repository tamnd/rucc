/* row: 3.5 sockaddr confusion */
/* allow */
void *malloc(unsigned long size);
void free(void *p);
/* Every network program does this, POSIX asks for it in so many words, and the model's
   compatible type relation has the sanctioned confusions written down rather than treating them
   as an exception somebody remembers to add later. */
struct sockaddr {
    unsigned short family;
    char data[14];
};

struct sockaddr_in {
    unsigned short family;
    unsigned short port;
    unsigned int address;
    char padding[8];
};

int bind_it(struct sockaddr *address) {
    return address->family;
}

int main(void) {
    struct sockaddr_in *in = malloc(sizeof(struct sockaddr_in));
    int family;
    in->family = 2;
    in->port = 80;
    family = bind_it((struct sockaddr *)in);
    free(in);
    return family == 2 ? 0 : 1;
}
