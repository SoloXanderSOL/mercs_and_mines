#!/usr/bin/env bash
# Stub harness for deploy/remote_deploy.sh (GAP-33). Runs the REAL script against PATH
# shims in a temp dir and checks every branch of its exit-code contract. CI runs this on
# every push; it needs bash, coreutils and sed, and no network, root or systemd.
#
# Model: a "binary" is a text file holding `sha=<x> healthy=<0|1> version=<mode>`.
# The systemctl shim's `restart` copies the current binary to $HARNESS_DIR/running; the
# curl shim answers from `running`. mv, cp, chmod and rm are real, so "rollback restores
# the old SHA" is proven by the script's own file moves, not asserted by a shim.
# version modes: ok (serves its SHA), 404 (a pre-/api/version build), nosha (200 with no
# build_sha). An unhealthy or absent `running` refuses every connection (curl exit 7).
set -uo pipefail

SCRIPT="$(cd "$(dirname "$0")" && pwd)/remote_deploy.sh"
HARNESS_DIR=$(mktemp -d)
trap 'rm -rf "$HARNESS_DIR"' EXIT
export HARNESS_DIR
mkdir -p "$HARNESS_DIR/bin"

cat > "$HARNESS_DIR/bin/sudo" <<'SHIM'
#!/usr/bin/env bash
exec "$@"
SHIM

cat > "$HARNESS_DIR/bin/systemctl" <<'SHIM'
#!/usr/bin/env bash
case "$1" in
  restart) cp "$APP_PATH/mercs-server" "$HARNESS_DIR/running" ;;
  reset-failed) exit 0 ;;
  *) echo "systemctl shim: unexpected '$*'" >&2; exit 64 ;;
esac
SHIM

cat > "$HARNESS_DIR/bin/journalctl" <<'SHIM'
#!/usr/bin/env bash
echo "[journalctl shim] $*"
SHIM

cat > "$HARNESS_DIR/bin/curl" <<'SHIM'
#!/usr/bin/env bash
url=""
for a in "$@"; do
  case "$a" in http*) url=$a ;; esac
done
run="$HARNESS_DIR/running"
[ -f "$run" ] || exit 7
field() { grep -o "$1=[^ ]*" "$run" | cut -d= -f2; }
[ "$(field healthy)" = 1 ] || exit 7
case "$url" in
  */api/units) echo '[]' ;;
  */api/version)
    case "$(field version)" in
      ok)    printf '{"build_sha":"%s","version":"0.3.0"}\n' "$(field sha)" ;;
      404)   exit 22 ;;
      nosha) echo '{"version":"0.3.0"}' ;;
      *)     exit 7 ;;
    esac ;;
  *) exit 22 ;;
esac
SHIM

chmod +x "$HARNESS_DIR"/bin/*

APP="$HARNESS_DIR/app"
PASSED=0
TOTAL=0
OUT=""
RC=0
FAILS=""

# reset <running|-> <current|-> <new>: "-" means the file is absent.
reset() {
  rm -rf "$APP" "$HARNESS_DIR/running"
  mkdir -p "$APP"
  [ "$1" = "-" ] || printf '%s\n' "$1" > "$HARNESS_DIR/running"
  [ "$2" = "-" ] || printf '%s\n' "$2" > "$APP/mercs-server"
  printf '%s\n' "$3" > "$APP/mercs-server.new"
}

deploy() {
  OUT=$(PATH="$HARNESS_DIR/bin:$PATH" APP_PATH="$APP" EXPECTED_SHA="$1" \
        TIMEOUT_SECS=2 POLL_SECS=0.1 bash "$SCRIPT" 2>&1)
  RC=$?
  FAILS=""
}

expect_rc()      { [ "$RC" -eq "$1" ] || FAILS="$FAILS; exit $RC, want $1"; }
expect_out()     { grep -qF -- "$1" <<<"$OUT" || FAILS="$FAILS; no line '$1'"; }
expect_no_out()  { ! grep -qF -- "$1" <<<"$OUT" || FAILS="$FAILS; unexpected '$1'"; }
expect_file()    {
  local got
  got=$(cat "$APP/$1" 2>/dev/null || echo "<absent>")
  [ "$got" = "$2" ] || FAILS="$FAILS; $1 = '$got', want '$2'"
}

verdict() {
  TOTAL=$((TOTAL + 1))
  if [ -z "$FAILS" ]; then
    PASSED=$((PASSED + 1))
    echo "ok   $1"
  else
    echo "FAIL $1${FAILS}"
    echo "----- script output -----"
    echo "$OUT"
    echo "-------------------------"
  fi
}

A="sha=A healthy=1 version=ok"
A_SICK="sha=A healthy=0 version=ok"
B="sha=B healthy=1 version=ok"
B_SICK="sha=B healthy=0 version=ok"
C="sha=C healthy=1 version=ok"
A_404="sha=A healthy=1 version=404"
A_NOSHA="sha=A healthy=1 version=nosha"

reset "$A" "$A" "$B"; deploy B
expect_rc 0; expect_out "DEPLOYED B"; expect_file mercs-server "$B"; expect_file mercs-server.prev "$A"
verdict "1 healthy deploy"

reset "$A" "$A" "$B_SICK"; deploy B
expect_rc 1; expect_out "ROLLED BACK to A (verified)"; expect_file mercs-server "$A"
verdict "2 unhealthy new build, rollback restores A"

reset "$A" "$A_SICK" "$B_SICK"; deploy B
expect_rc 2; expect_out "ROLLBACK FAILED"; expect_no_out "ROLLED BACK"; expect_file mercs-server "$A_SICK"
verdict "3 rollback target unhealthy, rollback fails loudly"

reset "$A" "$A" "$C"; deploy B
expect_rc 1; expect_out "ROLLED BACK to A (verified)"; expect_out "last seen: C"; expect_file mercs-server "$A"
verdict "4 healthy but wrong SHA, rolled back"

reset "-" "-" "$B_SICK"; deploy B
expect_rc 2; expect_out "no previous binary"; expect_out "ROLLBACK FAILED"; expect_no_out "ROLLED BACK"
verdict "5 first deploy unhealthy, no rollback possible"

reset "$A_404" "$A_404" "$B_SICK"; deploy B
expect_rc 1; expect_out "old SHA unknown"; expect_out "health only"; expect_no_out "ROLLED BACK to"
expect_file mercs-server "$A_404"
verdict "6 old build has no /api/version, health-only rollback"

reset "-" "$A" "$B"; deploy B
expect_rc 0; expect_out "old SHA unknown"; expect_out "DEPLOYED B"; expect_file mercs-server "$B"
verdict "7 service down before deploy"

reset "$A_NOSHA" "$A_NOSHA" "$B"; deploy B
expect_rc 0; expect_out "old SHA unknown"; expect_out "DEPLOYED B"; expect_file mercs-server "$B"
verdict "8 old /api/version without build_sha"

echo "$PASSED/$TOTAL passed"
[ "$PASSED" -eq "$TOTAL" ]
