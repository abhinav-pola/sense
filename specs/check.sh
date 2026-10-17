#!/usr/bin/env bash
# Model-checks every spec with TLC using the repo-local JDK.
#
#   specs/check.sh            check specs/*.cfg (models with the recommended
#                             fixes); exits non-zero on any violation.
#   specs/check.sh --bugs     also check specs/bugs/*.cfg (models of the
#                             current code); these reproduce known bugs, so
#                             they are expected to fail and make the exit
#                             code non-zero.
#   specs/check.sh NAME.cfg   check just that config (path relative to specs/).
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(dirname "$here")"
java="$root/.toolchain/jdk/Contents/Home/bin/java"
jar="$root/.toolchain/tla2tools.jar"
[[ -x "$java" ]] || { echo "missing $java (see specs/README.md)"; exit 2; }
[[ -f "$jar" ]] || { echo "missing $jar (see specs/README.md)"; exit 2; }

configs=()
if [[ $# -gt 0 && "$1" != "--bugs" ]]; then
    configs=("$@")
else
    for cfg in "$here"/*.cfg; do configs+=("${cfg#"$here"/}"); done
    if [[ "${1:-}" == "--bugs" ]]; then
        for cfg in "$here"/bugs/*.cfg; do configs+=("${cfg#"$here"/}"); done
    fi
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failed=0
for cfg in "${configs[@]}"; do
    # bugs/Foo_bar.cfg and Foo.cfg both check Foo.tla.
    base="$(basename "$cfg" .cfg)"
    module="${base%%_*}"
    printf '== %-45s ' "$cfg"
    log="$work/$base.log"
    (cd "$here" && "$java" -XX:+UseParallelGC -cp "$jar" tlc2.TLC \
        -workers auto -metadir "$work/$base.states" -cleanup \
        -config "$cfg" "$module.tla") >"$log" 2>&1
    status=$?
    states="$(grep -Eo '[0-9,]+ distinct states found' "$log" | tail -1)"
    if [[ $status -eq 0 ]] && grep -q 'No error has been found' "$log"; then
        echo "ok ($states)"
    else
        echo "FAILED (exit $status)"
        grep -E '^Error:|is violated|Temporal properties were violated|Deadlock' "$log" | head -5 | sed 's/^/   /'
        echo "   full log: kept at $here/.last/$base.log"
        mkdir -p "$here/.last" && cp "$log" "$here/.last/$base.log"
        failed=1
    fi
done
exit $failed
