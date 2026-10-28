import re,sys
L=open(sys.argv[1]).read().splitlines()
vt={}
for l in L:
    m=re.match(r"vt (0x[0-9a-f]+)\[(\d+)\] = (0x[0-9a-f]+)",l)
    if m: vt.setdefault(m.group(1),[]).append(m.group(3))
cols=sys.argv[2:]
n=min(len(vt[c.split('=')[0]]) for c in cols)
for i in range(n):
    print(i, " ".join(f"{c.split('=')[1]}={int(vt[c.split('=')[0]][i],16)-0x140000000:#x}" for c in cols))
