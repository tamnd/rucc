// Each statement form, and the way a `switch` becomes a table of cases beside a body whose
// labels point into it.

int loops(int n) {
  int total = 0;
  while (n > 0) {
    total += n;
    n--;
    if (total > 100) break;
    if (total == 7) continue;
  }
  for (int i = 0; i < 4; i++) {
    total += i;
  }
  for (;;) {
    break;
  }
  do {
    total++;
  } while (total < 2);
  return total;
}

int branches(int n) {
  if (n < 0) {
    return -1;
  } else if (n == 0) {
    return 0;
  } else {
    ;
  }
  switch (n) {
    case 0:
      return 100;
    case 1:
    case 2:
      return 200;
    case 4 ... 6:
      return 400;
    default:
      break;
  }
  goto done;
done:
  return n;
}

void nothing(void) {
  return;
}
