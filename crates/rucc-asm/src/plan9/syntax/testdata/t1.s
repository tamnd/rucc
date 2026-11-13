#include "inc.h"
#include "sub/deep.h"
#pragma lib "libc.a"
#pragma other stuff
TEXT	foo(SB), 7, $16
	MOVQ	$K, AX
	ADD3(AX, BX, CX)
	V(AX, BX)
	MOVQ	$DEEP, x+8(FP)
	MOVQ	$(1.5), X0
	MOVSD	$-2.25e3, X1
	MOVQ	$"abc\n\001", AX
	MOVQ	$'a', AX
	MOVQ	$0x7fffffffffff, AX
	MOVQ	$-0x80000000, AX
	MOVQ	$18446744073709551615, AX
	MOVQ	$0777, AX
	MOVQ	$09, AX
	LEAQ	foo<>+4(SB), SI
	MOVQ	(AX)(BX*4), BX
	MOVQ	-8(BP), AX
	MOVL	tab(SB)(CX*4), DX
	MOVQ	(SP), AX
	SHLQ	CX, AX:DX
	JMP	lab
	JEQ	lab+1
	JMP	*AX
	JMP	*8(AX)
	CALL	bar(SB)
	JMP	2(PC)
lab:
	NOP
	NOP	,
	NOP	AX
	C = (3*4+(7%3)-(~1)^5&6|8>>1<<2)
	MOVQ	$C, AX
#line 100 "fake.c"
	RET
#line 200 "fake.c"
	RETFL	$8
	DATA	tab+0(SB)/8, $foo(SB)
	DATA	tab+8(SB)/4, $"hi"
	GLOBL	tab(SB), $16
	CMPPS	X0, X1, 4
	PSHUFL	$3, X0, X1
	MOVQ	DX, foo(SB)
	MOVQ	foo(SB), foo(SB)
	MOVQ	$foo(SB), foo(SB)
	ADDQ	x(SB), y(SB)
	RET
