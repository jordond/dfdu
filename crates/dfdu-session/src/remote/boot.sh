# shellcheck disable=SC3067,SC2015 # test -O: dash, bash, busybox and BSD sh have it
# Starts the dfdu agent on this host: an installed one, one sent earlier, or one received on
# stdin (remote/boot.rs has the exchange). Reaches the host single-quoted through the login
# shell, so it holds no single quote or backslash. $1: our protocol version, $2: auto or upload.
p=$1 m=$2
echo
echo "DFDU-BOOT 1 $(uname -s) $(uname -m)"
runs() { "$1" agent --protocols 2>/dev/null | { read -r lo hi && [ "$lo" -le "$p" ] && [ "$p" -le "$hi" ]; } 2>/dev/null; }
# A directory of ours to keep agents in: not a symlink, owned by us.
ours() { [ -n "$1" ] && [ -d "$1" ] && [ ! -L "$1" ] && [ -O "$1" ]; }
if [ "$m" = auto ] && command -v dfdu >/dev/null 2>&1 && runs dfdu; then echo INSTALLED; exec dfdu agent; fi
echo "KEY?"
read -r w keys || exit 1
[ "$w" = KEY ] || exit 1
for k in $keys; do
  case $k in ""|*[!A-Za-z0-9._-]*) echo "ERR bad key"; exit 1;; esac
done
set -- "${XDG_CACHE_HOME:-$HOME/.cache}/dfdu/agent" "${XDG_RUNTIME_DIR:+$XDG_RUNTIME_DIR/dfdu-agent}" "/dev/shm/dfdu-agent-$(id -u)"
for k in $keys; do
  for d in "$@"; do
    if ours "$d" && runs "$d/$k/dfdu"; then touch "$d/$k" "$d/$k/dfdu"; echo HAVE; exec "$d/$k/dfdu" agent; fi
  done
done
dir=""
for d in "$@"; do
  [ -n "$d" ] && (umask 077; mkdir -p "$d") 2>/dev/null && ours "$d" && [ -w "$d" ] && dir=$d && break
done
[ -n "$dir" ] || { echo "ERR no directory of ours to keep the agent in"; exit 1; }
echo "SEND $dir"
read -r k size sum || exit 1
case $k in ""|*[!A-Za-z0-9._-]*) echo "ERR bad key"; exit 1;; esac
tmp=""
clean() { [ -z "$tmp" ] || rm -f "$tmp"; }
trap clean EXIT
trap "clean; exit 1" HUP INT TERM
tmp=$(mktemp "$dir/.dfdu.XXXXXX") || { echo "ERR cannot write in $dir"; exit 1; }
head -c "$size" > "$tmp" || { echo "ERR cannot write in $dir (disk full?)"; exit 1; }
got=$(($(wc -c < "$tmp")))
[ "$got" = "$size" ] || { echo "ERR received $got of $size bytes"; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then got=$(sha256sum < "$tmp"); elif command -v shasum >/dev/null 2>&1; then got=$(shasum -a 256 < "$tmp"); else got=$sum; fi
[ "${got%% *}" = "$sum" ] || { echo BAD; exit 1; }
(umask 077; mkdir -p "$dir/$k") && chmod 700 "$tmp" && mv -f "$tmp" "$dir/$k/dfdu" || { echo "ERR cannot write $dir/$k"; exit 1; }
tmp=""
bin=""
for d in "$@"; do
  [ -n "$d" ] || continue
  if [ "$d" != "$dir" ]; then
    (umask 077; mkdir -p "$d") 2>/dev/null && ours "$d" && (umask 077; mkdir -p "$d/$k") && tmp=$(mktemp "$d/$k/.dfdu.XXXXXX" 2>/dev/null) || continue
    cp "$dir/$k/dfdu" "$tmp" && chmod 700 "$tmp" && mv -f "$tmp" "$d/$k/dfdu" || { clean; tmp=""; continue; }
    tmp=""
  fi
  if runs "$d/$k/dfdu"; then bin=$d/$k/dfdu; break; fi
done
if [ -z "$bin" ]; then
  why=$("$dir/$k/dfdu" agent --protocols 2>&1 | head -n 1)
  echo "ERR the agent does not run there: ${why:-no output} (is its directory mounted noexec?)"
  exit 1
fi
for d in "$@"; do
  if ours "$d"; then
    find "$d" -mindepth 1 -maxdepth 1 -type d -mtime +30 -exec rm -rf {} + 2>/dev/null
    find "$d" -mindepth 1 -maxdepth 2 -name ".dfdu.*" -mtime +1 -exec rm -f {} + 2>/dev/null
  fi
done
echo OK
exec "$bin" agent
