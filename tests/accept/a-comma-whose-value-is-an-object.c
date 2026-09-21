/* accept: all */
/* What a comma expression is worth is the right side, and the right side may be a structure or a
   union rather than a number. janet is written this way and so is every library that gives up
   loudly: a call that does not return, then a value after it so that the arm of the conditional
   is worth something of the type the other arm is. The object is the one the right side named
   and not a copy of it, which is what makes reading a member of it read that object. */

struct pair {
	int a, b;
};

union either {
	double d;
	long n;
};

static int calls;

static int did(int n)
{
	calls += n;
	return n;
}

static struct pair a_pair(int a)
{
	struct pair p;

	p.a = a;
	p.b = a + 1;
	return p;
}

static union either a_union(long n)
{
	union either e;

	e.n = n;
	return e;
}

/* Returned, which is the object read into the registers it travels home in. */
static struct pair returned(int a)
{
	return (did(1), a_pair(a));
}

/* Assigned, which is the object copied into the variable. */
static struct pair assigned(int a)
{
	struct pair p = (did(2), a_pair(a));

	return p;
}

/* A member read out of one, which is the read that proves no copy was needed. */
static int a_member(int a)
{
	return (did(4), a_pair(a)).b;
}

/* The shape janet writes, which is an arm of a conditional worth a value after a call. */
static union either the_janet_shape(int c, long n)
{
	return c ? a_union(n) : (did(8), a_union(-n));
}

/* An object the program already has rather than one a call produced, which names that object
   and copies nothing on the way past the comma. */
static struct pair an_object_already(void)
{
	struct pair p = a_pair(20);

	return (did(16), p);
}

int main(void)
{
	if (returned(1).b != 2) {
		return 1;
	}
	if (assigned(3).a != 3) {
		return 2;
	}
	if (a_member(5) != 6) {
		return 3;
	}
	if (the_janet_shape(0, 7).n != -7) {
		return 4;
	}
	if (the_janet_shape(1, 7).n != 7) {
		return 5;
	}
	if (an_object_already().a != 20) {
		return 6;
	}
	/* Every left side ran, and each of them exactly once. */
	if (calls != 1 + 2 + 4 + 8 + 16) {
		return 7;
	}
	return 0;
}
