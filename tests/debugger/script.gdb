# What the debugger is asked, once it is standing in the frame above the call to `stop`.
#
# Each question is announced before it is asked, so that an answer is read by the name of the thing
# that was asked for rather than by the order the answers came back in. An answer that did not come
# back at all then reads as a missing answer instead of shifting every answer after it.
set confirm off
set pagination off
break stop
run
frame 1
echo ask count\n
print count
echo ask total\n
print total
echo ask inner\n
print inner
echo ask buf\n
print buf
echo ask through the pointer\n
print label[1]
echo ask early\n
print early
echo names\n
info locals
echo done\n
quit
