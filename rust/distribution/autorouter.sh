#!/bin/sh
# Native npm dispatcher: selection and exec only. No downloads or Node fallback.
set -eu
fail() { printf '%s\n' "AutoRouter: $1" >&2; exit 1; }
utility() {
  if [ -x "/usr/bin/$1" ]; then printf '/usr/bin/%s\n' "$1";
  elif [ -x "/bin/$1" ]; then printf '/bin/%s\n' "$1";
  else fail "required system utility $1 is unavailable."; fi
}
dirname_command=$(utility dirname)
readlink_command=$(utility readlink)
uname_command=$(utility uname)
case "$0" in
  */*) launcher=$0 ;;
  *) launcher=$(command -v "$0") || fail 'cannot locate the installed launcher.' ;;
esac
links=0
while [ -L "$launcher" ]; do
  links=$((links + 1))
  [ "$links" -le 40 ] || fail 'launcher symbolic links form a loop.'
  parent=$(CDPATH= cd -- "$("$dirname_command" -- "$launcher")" && pwd -P) || fail 'cannot resolve the launcher directory.'
  link=$("$readlink_command" "$launcher") || fail 'cannot read the launcher symbolic link.'
  case "$link" in /*) launcher=$link ;; *) launcher=$parent/$link ;; esac
done
root=$(CDPATH= cd -- "$("$dirname_command" -- "$launcher")/.." && pwd -P) || fail 'cannot resolve the installation directory.'
platform=$("$uname_command" -s) || fail 'cannot identify the operating system.'
architecture=$("$uname_command" -m) || fail 'cannot identify the CPU architecture.'
case "$platform:$architecture" in
  Darwin:arm64|Darwin:aarch64) target=aarch64-apple-darwin ;;
  Darwin:x86_64) target=x86_64-apple-darwin ;;
  Linux:x86_64|Linux:amd64)
    if [ -x "$root/native/x86_64-unknown-linux-musl/claude-autorouter" ]; then target=x86_64-unknown-linux-musl; else target=x86_64-unknown-linux-gnu; fi ;;
  Linux:aarch64|Linux:arm64)
    if [ -x "$root/native/aarch64-unknown-linux-musl/claude-autorouter" ]; then target=aarch64-unknown-linux-musl; else target=aarch64-unknown-linux-gnu; fi ;;
  Linux:armv7l|Linux:armv8l) target=armv7-unknown-linux-gnueabihf ;;
  Linux:ppc64le) target=powerpc64le-unknown-linux-gnu ;;
  Linux:s390x) target=s390x-unknown-linux-gnu ;;
  Linux:loongarch64) target=loongarch64-unknown-linux-gnu ;;
  Linux:riscv64) target=riscv64gc-unknown-linux-gnu ;;
  *) fail "no native artifact is available for $platform/$architecture." ;;
esac
binary=$root/native/$target/claude-autorouter
[ -f "$binary" ] && [ -x "$binary" ] || fail "the native artifact for $target is missing or not executable."
exec "$binary" "$@"
