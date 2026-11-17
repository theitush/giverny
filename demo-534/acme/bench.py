"""A short benchmark of greet(): two minutes of real CPU and ~300 MB."""
import time
from greet import greet

buf = bytearray(300 << 20)  # what a real benchmark's fixtures might hold
t0 = time.time()
n = 0
while time.time() - t0 < 120:
    for i in range(20000):
        greet(str(i), i % 2 == 0)
    n += 20000
print(f"{n} greetings in {time.time() - t0:.0f}s: {n / (time.time() - t0):,.0f}/s")
