-- The program for perf.sh. It runs for about a second and keeps the interpreter loop, the string
-- table and the hash table busy.
local t = {}
for i = 1, 300000 do
  t[i] = "s" .. i
end
local n = 0
for _ = 1, 20 do
  for i = 1, #t do
    n = n + #t[i]
  end
end
local h = {}
for i = 1, #t do
  h[t[i]] = i
end
print(n)
