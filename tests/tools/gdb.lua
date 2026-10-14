-- The program for gdb.sh. It must call each function that gdb.sh stops in.
local t = {}
for i = 1, 200 do
  t["k" .. i] = i
end
local s = ""
for i = 1, 200 do
  s = s .. string.char(65 + i % 26)
end
local n = 0
for i = 1, #s do
  n = n + s:byte(i)
end
collectgarbage("step")
print(n, t.k10, #s)
