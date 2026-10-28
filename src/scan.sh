# Lists the git repositories in the given folders, up to two levels deep, as
# "R:<path>", "T:<last git activity, unix seconds>", and "U:<origin url>"
# lines, then "DONE". A repository is a folder whose .git is a directory, so
# worktrees (whose .git is a file) are skipped, and the scan doesn't look
# inside repositories. The first argument is a space-separated list of folder
# names to skip. This is the remote twin of `discover::scan_local`.
exclude=$1
shift
skip() {
  name=${1%/}
  name=${name##*/}
  case $name in .* | node_modules | target) return 0 ;; esac
  for e in $exclude; do [ "$name" = "$e" ] && return 0; done
  return 1
}
report() {
  dir=${1%/}
  log=$dir/.git/logs/HEAD
  [ -f "$log" ] || log=$dir/.git/HEAD
  t=$(stat -c %Y "$log" 2>/dev/null || stat -f %m "$log" 2>/dev/null || echo 0)
  url=$(sed -n '/^\[remote "origin"\]/,/^\[/s/^[[:space:]]*url[[:space:]]*=[[:space:]]*//p' "$dir/.git/config" 2>/dev/null)
  printf 'R:%s\nT:%s\nU:%s\n' "$dir" "$t" "${url%%
*}"
}
for root in "$@"; do
  case $root in "~") root=$HOME ;; "~/"*) root=$HOME/${root#"~/"} ;; esac
  for d in "$root"/*/; do
    [ -d "$d" ] || continue
    skip "$d" && continue
    if [ -d "$d.git" ]; then
      report "$d"
      continue
    fi
    for c in "$d"*/; do
      [ -d "$c" ] || continue
      skip "$c" && continue
      [ -d "$c.git" ] && report "$c"
    done
  done
done
printf 'DONE\n'
