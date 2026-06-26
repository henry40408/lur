local out = {}
for line in lur.stdin.lines() do
    out[#out + 1] = line
end
lur.stdout.write(table.concat(out, ","))
