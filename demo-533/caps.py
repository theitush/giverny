#!/usr/bin/env python3
# caps.py <timeline.tsv>: where each caption starts and ends in the cut (output frames, 15 fps, after the title card), for picking stills.
import sys
ev=[]
for l in open(sys.argv[1]):
    p=l.rstrip('\n').split('\t',2)
    if len(p)<2: continue
    ev.append((p[0],int(p[1]),p[2] if len(p)>2 else ''))
ev.sort(key=lambda e:e[1])
t0=[e[1] for e in ev if e[0]=='start'][0]; t1=[e[1] for e in ev if e[0]=='end'][-1]
fps=15; n=int(3.5*fps); cap='';speed=1.0;cut=False;ei=0;s=float(t0)
caps=[]  # (startframe, cap)
while s<=t1:
    while ei<len(ev) and ev[ei][1]<=s:
        k,t,a=ev[ei]; ei+=1
        if k=='cap': cap=a; caps.append([n+1,None,a])
        elif k=='speed': speed=float(a)
        elif k=='cut': cut=True
        elif k=='uncut': cut=False
    if cut:
        nx=[e for e in ev[ei:] if e[0]=='uncut']
        if not nx: break
        s=float(nx[0][1]); continue
    n+=1
    s+=1000.0/fps*speed
for i,c in enumerate(caps):
    c[1]=(caps[i+1][0]-1) if i+1<len(caps) else n
for a,b,c in caps: print(a,b,f"{(a-1)/fps:.1f}s",c[:90])
print('last',n,'+card',n+int(3.5*fps))
