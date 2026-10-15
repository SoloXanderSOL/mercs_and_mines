#!/usr/bin/env bash
# Droplet-side deploy: back up, swap, restart, prove the new build is serving, and roll
# back if it is not. CI streams this file over ssh:
#
#   ssh host "APP_PATH='…' EXPECTED_SHA='…' bash -s" < deploy/remote_deploy.sh
#
# Exit codes are a contract (GAP-33):
#   0  deployed: the service reports build_sha == EXPECTED_SHA
#   1  deploy failed; the rollback was VERIFIED (by SHA when the old one was read,
#      otherwise by health only, and the message says which)
#   2  the rollback failed or was impossible — production may be down
#
# No `set -e`: every call that matters is checked where it is made, so a failure picks
# its own branch instead of aborting halfway through a swap.
# deploy/test_remote_deploy.sh runs this file against PATH shims; keep them in step.
set -uo pipefail

: "${APP_PATH:?APP_PATH must be set}"
: "${EXPECTED_SHA:?EXPECTED_SHA must be set}"
HEALTH_URL="${HEALTH_URL:-http://127.0.0.1:3000/api/units}"
VERSION_URL="${VERSION_URL:-http://127.0.0.1:3000/api/version}"
SERVICE="${SERVICE:-mercs-and-mines}"
TIMEOUT_SECS="${TIMEOUT_SECS:-60}"
POLL_SECS="${POLL_SECS:-2}"

CURRENT="$APP_PATH/mercs-server"
NEW="$APP_PATH/mercs-server.new"
PREV="$APP_PATH/mercs-server.prev"
LAST_SHA=none

# Reads the served build SHA into SHA. On any failure SHA is empty, SHA_ERR says why,
# and the function fails. A SHA is never assumed.
read_served_sha() {
  local body rc
  SHA=""
  body=$(curl -sf --max-time 5 "$VERSION_URL")
  rc=$?
  if [ "$rc" -ne 0 ]; then
    SHA_ERR="curl exit $rc from $VERSION_URL"
    return 1
  fi
  SHA=$(printf '%s' "$body" | sed -n 's/.*"build_sha":"\([^"]*\)".*/\1/p')
  if [ -z "$SHA" ]; then
    SHA_ERR="no build_sha in the response from $VERSION_URL"
    return 1
  fi
}

# Polls until the served SHA equals $1, within TIMEOUT_SECS. LAST_SHA keeps the last
# SHA seen, or "none".
wait_for_sha() {
  local want=$1
  local deadline=$(( SECONDS + TIMEOUT_SECS ))
  LAST_SHA=none
  while :; do
    if read_served_sha; then
      LAST_SHA=$SHA
      [ "$SHA" = "$want" ] && return 0
    fi
    [ "$SECONDS" -ge "$deadline" ] && return 1
    sleep "$POLL_SECS"
  done
}

# Polls until HEALTH_URL answers 200, within TIMEOUT_SECS. Used only when the old
# build's SHA could not be read (a pre-/api/version binary, or the service was down).
wait_for_health() {
  local deadline=$(( SECONDS + TIMEOUT_SECS ))
  while ! curl -sf --max-time 5 "$HEALTH_URL" >/dev/null 2>&1; do
    [ "$SECONDS" -ge "$deadline" ] && return 1
    sleep "$POLL_SECS"
  done
}

rollback_failed() {
  echo "::error::ROLLBACK FAILED: $1 Production may be down."
  exit 2
}

# Never prints ROLLED BACK before the re-poll succeeds, and never names a SHA it did
# not read.
rollback() {
  [ -f "$PREV" ] || rollback_failed "no previous binary to restore."
  echo "Restoring mercs-server.prev..."
  mv "$PREV" "$CURRENT" || rollback_failed "could not move mercs-server.prev into place."
  chmod +x "$CURRENT" || rollback_failed "could not make the restored binary executable."
  sudo systemctl reset-failed "$SERVICE" || true
  sudo systemctl restart "$SERVICE" || rollback_failed "systemctl restart failed after the restore."

  if [ -n "$OLD_SHA" ]; then
    if wait_for_sha "$OLD_SHA"; then
      echo "::error::Deploy failed; ROLLED BACK to $OLD_SHA (verified)"
      exit 1
    fi
    rollback_failed "previous binary did not come back as $OLD_SHA (last seen: $LAST_SHA; if the restart ran, the on-disk .prev may differ from what was running)."
  fi
  if wait_for_health; then
    echo "::error::Deploy failed; ROLLED BACK (previous build's SHA unknown; health only)"
    exit 1
  fi
  rollback_failed "previous binary did not answer $HEALTH_URL within ${TIMEOUT_SECS}s."
}

deploy_failed() {
  echo "::error::DEPLOY FAILED — $1"
  echo "----- last 50 journal lines -----"
  sudo journalctl -u "$SERVICE" -n 50 --no-pager || true
  echo "---------------------------------"
  rollback
}

if [ ! -f "$NEW" ]; then
  echo "::error::$NEW not found — nothing to deploy. The running service was not touched."
  exit 2
fi

# 1. The running build's SHA, read before anything changes.
OLD_SHA=""
if read_served_sha; then
  OLD_SHA=$SHA
  echo "Running build: $OLD_SHA"
else
  echo "old SHA unknown ($SHA_ERR); a rollback can be verified by health only."
fi

# 2. Back up. A stale .prev is removed even when there is no current binary, so a
# rollback can never restore a binary older than the one that was running.
if ! rm -f "$PREV"; then
  echo "::error::could not remove the stale mercs-server.prev. The running service was not touched."
  exit 2
fi
if [ -f "$CURRENT" ]; then
  if ! cp -a "$CURRENT" "$PREV"; then
    echo "::error::could not back up the current binary. The running service was not touched."
    exit 2
  fi
  echo "Backed up current binary -> mercs-server.prev"
else
  echo "no previous binary; rollback unavailable"
fi

# 3. Swap.
mv "$NEW" "$CURRENT" || deploy_failed "could not move mercs-server.new into place."
chmod +x "$CURRENT" || deploy_failed "could not make the new binary executable."

# 4. Restart. `systemctl restart` succeeds once the process forks, so it proves nothing
# by itself; the SHA poll below is the evidence. main.rs connects Postgres and Redis and
# runs every migration before it binds, so a served SHA proves the whole startup path.
sudo systemctl reset-failed "$SERVICE" || true
sudo systemctl restart "$SERVICE" || deploy_failed "systemctl restart failed."

# 5. Prove the new build is the one serving.
echo "Polling $VERSION_URL for build_sha $EXPECTED_SHA, up to ${TIMEOUT_SECS}s..."
if wait_for_sha "$EXPECTED_SHA"; then
  echo "DEPLOYED $EXPECTED_SHA"
  exit 0
fi
deploy_failed "the service did not report build_sha $EXPECTED_SHA within ${TIMEOUT_SECS}s (last seen: $LAST_SHA)."
