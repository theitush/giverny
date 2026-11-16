# The "tests" tab: a test run that never ends, busy about a third of one core.
import itertools, time
mods = ["panel", "rail", "feed", "relay", "page", "away", "jobs", "stop"]
for i in itertools.count(1):
    t = time.time()
    while time.time() - t < 0.3:
        pass
    print(f"test {mods[i % len(mods)]}::case_{i:04d} ... \033[32mok\033[0m", flush=True)
    time.sleep(0.7)
