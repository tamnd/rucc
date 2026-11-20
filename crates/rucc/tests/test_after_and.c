/* The digit loops of add_abs and sub_abs from Postgres REL_18_6 numeric.c, with the buffers passed in. See test_after_and.rs. */
typedef short NumericDigit;

#define NBASE 10000

void add_digits(const NumericDigit *var1digits, int var1ndigits, const NumericDigit *var2digits, int var2ndigits, NumericDigit *res_digits, int res_ndigits, int i1, int i2)
{
	int carry = 0;

	for (int i = res_ndigits - 1; i >= 0; i--)
	{
		i1--;
		i2--;
		if (i1 >= 0 && i1 < var1ndigits)
			carry += var1digits[i1];
		if (i2 >= 0 && i2 < var2ndigits)
			carry += var2digits[i2];

		if (carry >= NBASE)
		{
			res_digits[i] = carry - NBASE;
			carry = 1;
		}
		else
		{
			res_digits[i] = carry;
			carry = 0;
		}
	}
}

void sub_digits(const NumericDigit *var1digits, int var1ndigits, const NumericDigit *var2digits, int var2ndigits, NumericDigit *res_digits, int res_ndigits, int i1, int i2)
{
	int borrow = 0;

	for (int i = res_ndigits - 1; i >= 0; i--)
	{
		i1--;
		i2--;
		if (i1 >= 0 && i1 < var1ndigits)
			borrow += var1digits[i1];
		if (i2 >= 0 && i2 < var2ndigits)
			borrow -= var2digits[i2];

		if (borrow < 0)
		{
			res_digits[i] = borrow + NBASE;
			borrow = -1;
		}
		else
		{
			res_digits[i] = borrow;
			borrow = 0;
		}
	}
}

#ifdef RUN
int printf(const char *, ...);

int main(void)
{
	/* 9999.9999 + 1 and 100000000 - 0.0001, in base ten thousand digits */
	NumericDigit a[] = {9999, 9999}, b[] = {1}, c[] = {1, 0, 0}, d[] = {1};
	NumericDigit sum[3], diff[4];

	add_digits(a, 2, b, 1, sum, 3, 2, 2);
	printf("%d %d %d\n", sum[0], sum[1], sum[2]);
	sub_digits(c, 3, d, 1, diff, 4, 4, 1);
	printf("%d %d %d %d\n", diff[0], diff[1], diff[2], diff[3]);
	return 0;
}
#endif
