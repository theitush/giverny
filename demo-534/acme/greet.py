"""acme: a tiny greeter."""
import sys


def greet(name: str, shout: bool = False) -> str:
    line = f"Hello, {name}!"
    return line.upper() if shout else line


if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if a != "--shout"]
    print(greet(args[0] if args else "world", "--shout" in sys.argv))
