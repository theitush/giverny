# The "queue" tab: a worker loop at a few percent of a core.
import time
n = 0
while True:
    t = time.time()
    while time.time() - t < 0.05:
        pass
    n += 1
    if n % 20 == 0:
        print(f"queue: {n} jobs done", flush=True)
    time.sleep(0.45)
