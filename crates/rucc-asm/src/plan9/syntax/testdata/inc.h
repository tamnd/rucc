#define	ADD3(a,b,c)	ADDQ a, b; ADDQ b, c
#define	K	(1<<4|3)
#define V(x, ...) MOVQ x, __VA_ARGS__
