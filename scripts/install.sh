#!/bin/sh
# Giverny installer for Linux and macOS.
#
#   curl -fsSL https://github.com/y0av/giverny/releases/latest/download/install.sh | sh
#
# Downloads the release binary for this platform, installs it to
# ~/.local/bin (override with GIVERNY_BIN_DIR), and — on Linux — registers
# the desktop entry so the launcher shows an icon.
set -eu

REPO="y0av/giverny"
BIN_DIR="${GIVERNY_BIN_DIR:-$HOME/.local/bin}"
VERSION="${GIVERNY_VERSION:-latest}"

# Colour and drawing only for someone watching: a real terminal, a UTF-8
# locale, no NO_COLOR. Logs, CI and pipes get the same plain lines as ever.
fancy=
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-dumb}" != dumb ]; then
  case "${LC_ALL:-${LC_CTYPE:-${LANG:-}}}" in
    *UTF-8* | *utf-8* | *UTF8* | *utf8*) fancy=1 ;;
  esac
fi
esc=$(printf '\033')
reset= dim= bold= green= red= gold= wisteria= truecolor=
if [ -n "$fancy" ]; then
  reset="${esc}[0m" dim="${esc}[2m" bold="${esc}[1m"
  green="${esc}[32m" red="${esc}[31m" gold="${esc}[33m"
  case "${COLORTERM:-}" in
    truecolor | 24bit) truecolor=1 wisteria="${esc}[38;2;154;134;184m" ;;
    *) wisteria="${esc}[35m" ;;
  esac
fi

say() { printf '%s\n' "$*"; }
die() {
  if [ -n "$fancy" ]; then
    printf '  %s✗ error:%s %s\n' "$red" "$reset" "$*" >&2
  else
    printf 'error: %s\n' "$*" >&2
  fi
  exit 1
}
# A finished step. Plain output says `plain` instead, or nothing without one.
ok() {
  if [ -n "$fancy" ]; then
    printf '  %s✓%s %s\n' "$green" "$reset" "$1"
  elif [ -n "${2:-}" ]; then
    say "$2"
  fi
}

# The splash's wordmark (crates/app/src/splash.rs), washing from wisteria to
# cream down the letters. A test there keeps the two drawings identical.
wordmark() {
  set -- \
    '135;116;164| ███  █████ █   █ █████ ████  █   █ █   █' \
    '154;134;184|█   █   █   █   █ █     █   █ ██  █ █   █' \
    '168;151;194|█       █   █   █ █     █   █ ██  █  █ █' \
    '182;166;206|█ ███   █   █   █ ████  ████  █ █ █   █' \
    '195;182;216|█   █   █   █   █ █     █ █   █  ██   █' \
    '213;203;226|█   █   █    █ █  █     █  █  █  ██   █' \
    '231;224;238| ███  █████   █   █████ █   █ █   █   █'
  printf '\n'
  for row in "$@"; do
    if [ -n "$truecolor" ]; then
      printf '  %s[38;2;%sm%s%s\n' "$esc" "${row%%|*}" "${row#*|}" "$reset"
    else
      printf '  %s%s%s%s\n' "$bold" "$wisteria" "${row#*|}" "$reset"
    fi
  done
  printf '\n'
}

# What a binary says it is, or nothing. With no display to open, a build too
# old to know `--version` fails instead of starting its window.
version_of() {
  [ -x "$1" ] || return 0
  (unset DISPLAY WAYLAND_DISPLAY; "$1" --version) 2>/dev/null | sed -n 's/^giverny //p'
}

need() { command -v "$1" >/dev/null 2>&1 || die "this installer needs $1"; }
need uname
need tar

[ -z "$fancy" ] || wordmark

case "$(uname -s)" in
  Linux)  os=unknown-linux-gnu ;;
  Darwin) os=apple-darwin ;;
  *) die "unsupported OS $(uname -s). Build from source: cargo install --path crates/app" ;;
esac

case "$(uname -m)" in
  x86_64|amd64) arch=x86_64 ;;
  arm64|aarch64) arch=aarch64 ;;
  *) die "unsupported architecture $(uname -m)" ;;
esac

target="${arch}-${os}"
asset="giverny-${target}.tar.gz"
if [ "$VERSION" = latest ]; then
  url="https://github.com/${REPO}/releases/latest/download/${asset}"
else
  url="https://github.com/${REPO}/releases/download/${VERSION}/${asset}"
fi
ok "$target"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

missing="no ${target} build in this release. Build from source instead:
  git clone https://github.com/${REPO} && cd giverny && cargo install --path crates/app"
if [ -n "$fancy" ]; then
  printf '  %s↓%s %s\n' "$wisteria" "$reset" "$asset"
else
  say "downloading ${asset}"
fi
if command -v curl >/dev/null 2>&1; then
  if [ -n "$fancy" ]; then
    # curl draws its own bar on stderr; this gives it the mark's colour.
    got=1
    printf '%s' "$wisteria" >&2
    curl -fSL --progress-bar "$url" -o "$tmp/$asset" || got=
    printf '%s' "$reset" >&2
    [ -n "$got" ] || die "$missing"
  else
    curl -fsSL "$url" -o "$tmp/$asset" || die "$missing"
  fi
elif command -v wget >/dev/null 2>&1; then
  wget -qO "$tmp/$asset" "$url" || die "$missing"
else
  die "this installer needs curl or wget"
fi

tar -xzf "$tmp/$asset" -C "$tmp"
[ -f "$tmp/giverny" ] || die "archive did not contain a giverny binary"

# The version being replaced is the one this tab's Giverny is running, when
# there is one; the file on disk can be a different build.
if [ "${TERM_PROGRAM:-}" = giverny ] && [ -n "${TERM_PROGRAM_VERSION:-}" ]; then
  old="$TERM_PROGRAM_VERSION"
else
  old=$(version_of "$BIN_DIR/giverny")
fi
new=$(version_of "$tmp/giverny")

mkdir -p "$BIN_DIR"
# Replace via a temp file + mv: an atomic rename works even if the old
# binary is currently running.
mv "$tmp/giverny" "$BIN_DIR/giverny.new"
chmod +x "$BIN_DIR/giverny.new"
mv -f "$BIN_DIR/giverny.new" "$BIN_DIR/giverny"
ok "installed $BIN_DIR/giverny" "installed $BIN_DIR/giverny"

if [ "$os" = unknown-linux-gnu ]; then
  if "$BIN_DIR/giverny" install-desktop >/dev/null 2>&1; then
    ok "desktop entry, icons and OOM policy" "registered the desktop entry, icons and OOM policy"
  fi
fi

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    say ""
    say "$BIN_DIR is not on your PATH. Add this to your shell profile:"
    say "  export PATH=\"$BIN_DIR:\$PATH\""
    ;;
esac

say ""
if [ -z "$fancy" ]; then
  say "run: giverny        (and 'giverny doctor' if Claude states look wrong)"
  exit 0
fi
if [ -n "$new" ] && [ -n "$old" ] && [ "$old" != "$new" ]; then
  say "  ${dim}${old} →${reset} ${gold}${bold}${new}${reset}"
elif [ -n "$new" ] && [ "$old" = "$new" ]; then
  say "  ${dim}reinstalled${reset} ${gold}${bold}${new}${reset}"
elif [ -n "$new" ]; then
  say "  ${dim}installed${reset} ${gold}${bold}${new}${reset}"
fi
if [ "${TERM_PROGRAM:-}" = giverny ]; then
  say "  ${dim}restart from the rail to finish${reset}"
else
  say "  ${dim}run:${reset} giverny ${dim}(and 'giverny doctor' if Claude states look wrong)${reset}"
fi
say ""
