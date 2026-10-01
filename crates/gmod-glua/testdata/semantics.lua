// differential test: GLua semantics
local out = {}
local function emit(...) out[#out + 1] = table.concat({...}, ",") end

-- operators and precedence
local a, b, c = 1, 2, nil
emit(tostring(a != b), tostring(!c), tostring(!a == false), tostring(a == 1 && b == 2 || c), tostring(!(a && c)))
emit(tostring(1 != 1 || !nil && 3), tostring(not nil != false))

/* continue in numeric for, with locals after it */
local s = 0
for i = 1, 10 do
	if i % 3 == 0 then continue end
	local sq = i * i
	s = s + sq
end
emit("for", s)

-- generic for, nested, continue in both levels, closures capturing loop locals
local fns = {}
for k, v in ipairs({ 5, 6, 7, 8 }) do
	if v == 6 then continue end
	for j = 1, 3 do
		if j == 2 then continue end
		local captured = v * 10 + j
		fns[#fns + 1] = function() return captured end
	end
	if k == 4 then continue end
	emit("outer", k)
end
for _, f in ipairs(fns) do emit("fn", f()) end

-- while with continue and break
local i, w = 0, 0
while true do
	i = i + 1
	if i > 20 then break end
	if i % 2 == 1 then continue end
	w = w + i
end
emit("while", w)

-- repeat-until with continue (no locals declared after the continue)
local r, n = 0, 0
repeat
	n = n + 1
	if n % 4 == 0 then continue end
	r = r + n
until n >= 12
emit("repeat", r)

-- continue as a name (soft keyword)
local t = { continue = 3 }
t.continue = t.continue + 1
local continue = t.continue
emit("name", continue)

-- return as the last statement after continue
local function first_odd(list)
	for _, x in ipairs(list) do
		if x % 2 == 0 then continue end
		return x
	end
end
emit("first_odd", first_odd({ 2, 4, 9, 11 }))

print(table.concat(out, "\n"))
