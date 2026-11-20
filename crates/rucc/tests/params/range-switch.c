extern int yes(void);
extern int no(void);
int shared(unsigned x)
{
  if (x == 1260 || x == 1261 || x == 1262 || x == 2964 || x == 2965 || x == 3000 || x == 5000 || x == 5001)
    return yes();
  return no();
}
