# The "db" tab: holds 350 MB, a little CPU.
import time
cache = bytearray(b"x" * (350 * 2**20))
print("db: 350 MB cached, serving on :5433", flush=True)
while True:
    t = time.time()
    while time.time() - t < 0.04:
        pass
    time.sleep(0.36)
