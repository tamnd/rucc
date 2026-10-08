/* The carry loop of accum_sum_carry and the inner loops of mul_var and accum_sum_add from Postgres REL_18_6 numeric.c. See loop_rotate.rs. */
typedef unsigned long uint64;
typedef unsigned int uint32;
typedef int int32;
typedef short NumericDigit;

#define NBASE 10000

int32 carry_inner(int32 *dig, int ndigits)
{
	int32 carry = 0;
	int32 newdig = 0;

	for (int i = ndigits - 1; i >= 0; i--)
	{
		newdig = dig[i] + carry;
		if (newdig >= NBASE)
		{
			carry = newdig / NBASE;
			newdig -= carry * NBASE;
		}
		else
			carry = 0;
		dig[i] = newdig;
	}
	return newdig;
}

void mul_inner(uint64 *dig, const uint32 *var2digitpairs, uint32 var1digitpair, int i1, int i2limit)
{
	uint64 *dig_i1_off = &dig[i1];

	for (int i2 = 0; i2 < i2limit; i2++)
		dig_i1_off[i2] += (uint64) var1digitpair * var2digitpairs[i2];
}

void accum_inner(int32 *accum_digits, const NumericDigit *val_digits, int val_ndigits, int i)
{
	for (int val_i = 0; val_i < val_ndigits; val_i++)
	{
		accum_digits[i] += (int32) val_digits[val_i];
		i++;
	}
}
