#!/usr/bin/env bash
# Non-destructive probe: does POST /api/v0/comments accept our JWT?
#
# Posts a well-formed body with a fake (but valid-shape) proposalId.
# No real comment is ever created because the proposal doesn't exist.
#
# Status codes we care about:
#   404            -> auth + role OK, only proposal lookup failed (you can comment)
#   400 or 422     -> body validation failed before role check; auth still passed
#   403            -> role-gated (DRep or other); design has to change
#   401            -> JWT rejected
#
# Usage: ./probe-auth.sh <jwt>
#   or set EKKLESIA_JWT in env and run with no args.

set -euo pipefail

JWT="${1:-${EKKLESIA_JWT:-}}"
if [[ -z "$JWT" ]]; then
  echo "error: pass JWT as arg or set EKKLESIA_JWT" >&2
  exit 2
fi

BASE="${EKKLESIA_BASE:-https://hydra-voting.intersectmbo.org}"
# 24-char hex, valid shape but unlikely to exist
FAKE_PROPOSAL_ID="ffffffffffffffffffffffff"

echo "POST $BASE/api/v0/comments with fake proposalId=$FAKE_PROPOSAL_ID"
echo "---"

# Capture body + status separately
HTTP_CODE=$(/usr/bin/curl -sS \
  -o /tmp/probe-body.$$ \
  -w "%{http_code}" \
  -X POST "$BASE/api/v0/comments" \
  -H "Content-Type: application/json" \
  -H "Origin: $BASE" \
  -H "Authorization: Bearer $JWT" \
  -H "Cookie: token=$JWT" \
  -d "{\"proposalId\":\"$FAKE_PROPOSAL_ID\",\"content\":\"probe — should never persist\"}")

echo "HTTP $HTTP_CODE"
echo "---"
/bin/cat /tmp/probe-body.$$
echo
/bin/rm -f /tmp/probe-body.$$

case "$HTTP_CODE" in
  404) echo "==> Auth + role passed. The proposal lookup failed (as expected). You CAN post comments." ;;
  400|422) echo "==> Body validation failed; auth seems to have passed. Likely fine." ;;
  403) echo "==> ROLE-GATED. The API rejects your account for posting comments." ;;
  401) echo "==> JWT rejected. Token may be expired or wrong cookie value." ;;
  *)   echo "==> Unexpected status. Read the body above." ;;
esac
