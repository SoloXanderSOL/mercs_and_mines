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
# Knobs (INF-6a): FAIL_RESTART=<n> fails the n-th restart; SUDO_DENY=1 refuses every
# privileged call. app/ and app.new/ each hold an index.html naming the front-end.
set -uo pipefail

SCRIPT="$(cd "$(dirname "$0")" && pwd)/remote_deploy.sh"
HARNESS_DIR=$(mktemp -d)
trap 'rm -rf "$HARNESS_DIR"' EXIT
export HARNESS_DIR
mkdir -p "$HARNESS_DIR/bin"

# sudo: strips -n, answers -l <cmd> itself (allowed), and runs anything else. With
# SUDO_DENY=1 it refuses everything, -l included, like a missing sudoers rule.
cat > "$HARNESS_DIR/bin/sudo" <<'SHIM'
#!/usr/bin/env bash
[ "${1:-}" = "-n" ] && shift
if [ "${SUDO_DENY:-0}" = 1 ]; then
  echo "sudo shim: refused '$*'" >&2
  exit 1
fi
[ "${1:-}" = "-l" ] && exit 0
exec "$@"
SHIM

# systemctl: counts restarts in $HARNESS_DIR/restarts. With FAIL_RESTART=<n>, the n-th
# restart fails without copying, so the old build keeps running (0 = never fail).
cat > "$HARNESS_DIR/bin/systemctl" <<'SHIM'
#!/usr/bin/env bash
case "$1" in
  restart)
    n=$(( $(cat "$HARNESS_DIR/restarts" 2>/dev/null || echo 0) + 1 ))
    echo "$n" > "$HARNESS_DIR/restarts"
    if [ "${FAIL_RESTART:-0}" = "$n" ]; then
      echo "systemctl shim: restart $n fails" >&2
      exit 1
    fi
    cp "$APP_PATH/mercs-server" "$HARNESS_DIR/running" ;;
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

# reset <running|-> <current|-> <new> <app|-> <app.new|->: "-" means absent. app/ and
# app.new/ each hold one file, index.html, whose content names the front-end.
reset() {
  rm -rf "$APP" "$HARNESS_DIR/running" "$HARNESS_DIR/restarts"
  mkdir -p "$APP"
  [ "$1" = "-" ] || printf '%s\n' "$1" > "$HARNESS_DIR/running"
  [ "$2" = "-" ] || printf '%s\n' "$2" > "$APP/mercs-server"
  printf '%s\n' "$3" > "$APP/mercs-server.new"
  if [ "$4" != "-" ]; then mkdir -p "$APP/app"; printf '%s\n' "$4" > "$APP/app/index.html"; fi
  if [ "$5" != "-" ]; then mkdir -p "$APP/app.new"; printf '%s\n' "$5" > "$APP/app.new/index.html"; fi
}

# deploy <expected sha>. FAIL_RESTART and SUDO_DENY reach the shims when set on the call.
deploy() {
  OUT=$(PATH="$HARNESS_DIR/bin:$PATH" APP_PATH="$APP" EXPECTED_SHA="$1" \
        FAIL_RESTART="${FAIL_RESTART:-0}" SUDO_DENY="${SUDO_DENY:-0}" \
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
expect_present() { [ -e "$APP/$1" ] || FAILS="$FAILS; $1 is absent, want present"; }
expect_absent()  { [ ! -e "$APP/$1" ] || FAILS="$FAILS; $1 is present, want absent"; }

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

APP_A="app A"
APP_B="app B"

reset "$A" "$A" "$B" "$APP_A" "$APP_B"; deploy B
expect_rc 0; expect_out "DEPLOYED B"; expect_file mercs-server "$B"; expect_file mercs-server.prev "$A"
expect_file app/index.html "$APP_B"; expect_file app.prev/index.html "$APP_A"
verdict "1 healthy deploy"

reset "$A" "$A" "$B_SICK" "$APP_A" "$APP_B"; deploy B
expect_rc 1; expect_out "ROLLED BACK to A (verified)"; expect_file mercs-server "$A"
verdict "2 unhealthy new build, rollback restores A"

reset "$A" "$A_SICK" "$B_SICK" "$APP_A" "$APP_B"; deploy B
expect_rc 2; expect_out "ROLLBACK FAILED"; expect_no_out "ROLLED BACK"; expect_file mercs-server "$A_SICK"
verdict "3 rollback target unhealthy, rollback fails loudly"

reset "$A" "$A" "$C" "$APP_A" "$APP_B"; deploy B
expect_rc 1; expect_out "ROLLED BACK to A (verified)"; expect_out "last seen: C"; expect_file mercs-server "$A"
verdict "4 healthy but wrong SHA, rolled back"

reset "-" "-" "$B_SICK" "$APP_A" "$APP_B"; deploy B
expect_rc 2; expect_out "no previous binary"; expect_out "ROLLBACK FAILED"; expect_no_out "ROLLED BACK"
verdict "5 first deploy unhealthy, no rollback possible"

reset "$A_404" "$A_404" "$B_SICK" "$APP_A" "$APP_B"; deploy B
expect_rc 1; expect_out "old SHA unknown"; expect_out "health only"; expect_no_out "ROLLED BACK to"
expect_file mercs-server "$A_404"
verdict "6 old build has no /api/version, health-only rollback"

reset "-" "$A" "$B" "$APP_A" "$APP_B"; deploy B
expect_rc 0; expect_out "old SHA unknown"; expect_out "DEPLOYED B"; expect_file mercs-server "$B"
verdict "7 service down before deploy"

reset "$A_NOSHA" "$A_NOSHA" "$B" "$APP_A" "$APP_B"; deploy B
expect_rc 0; expect_out "old SHA unknown"; expect_out "DEPLOYED B"; expect_file mercs-server "$B"
verdict "8 old /api/version without build_sha"

# GAP-55: the new build's restart is refused; the old process never stopped.
reset "$A" "$A" "$B" "$APP_A" "$APP_B"; FAIL_RESTART=1 deploy B
expect_rc 1; expect_out "systemctl restart failed."; expect_out "ROLLED BACK to A (verified)"
expect_file mercs-server "$A"
verdict "9 new-build restart fails, rollback verified"

reset "$A" "$A" "$B_SICK" "$APP_A" "$APP_B"; FAIL_RESTART=2 deploy B
expect_rc 2; expect_out "ROLLBACK FAILED"; expect_out "systemctl restart failed after the restore."
expect_no_out "ROLLED BACK"
verdict "10 rollback restart fails, rollback fails loudly"

# GAP-55: a refused sudo is caught by the probe before anything changes.
reset "$A" "$A" "$B" "$APP_A" "$APP_B"; SUDO_DENY=1 deploy B
expect_rc 2; expect_file mercs-server "$A"; expect_present mercs-server.new; expect_absent mercs-server.prev
expect_file app/index.html "$APP_A"; expect_present app.new
verdict "11 sudo refused at the probe, nothing touched"

# GAP-54: a rollback restores app/ with the binary.
reset "$A" "$A" "$B_SICK" "$APP_A" "$APP_B"; deploy B
expect_rc 1; expect_file app/index.html "$APP_A"; expect_file mercs-server "$A"
verdict "12 rollback restores app/"

reset "$A" "$A" "$B" "$APP_A" "-"; deploy B
expect_rc 2; expect_file mercs-server "$A"; expect_present mercs-server.new; expect_absent mercs-server.prev
verdict "13 app.new/ missing, nothing touched"

echo "$PASSED/$TOTAL passed"
[ "$PASSED" -eq "$TOTAL" ]
