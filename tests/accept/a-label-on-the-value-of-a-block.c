/* accept: all */
/* A statement expression is worth what its last statement is worth, and a label in front of that
   statement does not take the value away. The shape that matters is the one an interpreter is
   written with: a block whose value is the address of a label defined inside it, which is how
   tcc's own test file checks that computed goto works. A compiler that only reads the value off
   a bare expression statement calls all of these `void` and then refuses every use of them. */

extern int use(int);

/* The value is the address of a label of the block itself. */
static void *the_label_inside(void)
{
	return ({
		__label__ here;
	      here:
		&&here;
	});
}

/* More than one label on the same statement, each of which is looked through. */
static int two_labels(int c)
{
	return ({
	      first:
	      second:
		c + 1;
	});
}

/* A jump to the label the value is taken at, so the value comes out of the block the label
   started rather than the one the statement expression opened in. */
static int a_label_jumped_back_to(int n)
{
	int t = 0;
	return ({
		__label__ again;
	      again:
		t++, t < n ? ({ goto again; }) : (void)0, t;
	});
}

/* And the plain cases, which have to keep working: no label at all, and a label on a statement
   that is not the last one. */
static int no_label(int c)
{
	return ({ c + 2; });
}

static int a_label_further_up(int n)
{
	int t = 0;
	return ({
		__label__ again;
	      again:
		t++;
		if (t < n)
			goto again;
		t;
	});
}

int main(void)
{
	int bad = 0;
	bad += the_label_inside() == 0;
	bad += two_labels(1) != 2;
	bad += a_label_jumped_back_to(4) != 4;
	bad += no_label(1) != 3;
	bad += a_label_further_up(3) != 3;
	return bad + use(0) * 0;
}
