# Vendored upstream OpenAPI specs

These YAMLs are mirrors of the authoritative Ekklesia API specs published at
[`lerna-labs/ekklesia-docs`](https://github.com/lerna-labs/ekklesia-docs).
They are the source of truth Concordance implementations follow — when the
client behaves differently from these files, the client is wrong.

## Files

| Local file | Upstream path |
|---|---|
| `proposals-openapi.yaml` | `api/proposals/openapi.yaml` — exercised today by `list_proposals`, `get_proposal`, comments, etc. |
| `voting-v1-openapi.yaml` | `api/voting/openapi.v1.yaml` — Hydra-backed voting surface. Not yet used; queued for v0.4 work. |

Each vendored file carries a one-line header naming the source repo, the
fetch date, and the upstream commit SHA. Bodies below that header are
byte-identical to upstream.

## Refreshing

Re-fetch from upstream HEAD and overwrite in place:

```sh
SHA=$(gh api repos/lerna-labs/ekklesia-docs/commits/HEAD --jq '.sha')
DATE=$(date -u +%Y-%m-%d)
for src in api/proposals/openapi.yaml api/voting/openapi.v1.yaml; do
  case "$src" in
    api/proposals/*) dst=docs/upstream/proposals-openapi.yaml ;;
    api/voting/*)    dst=docs/upstream/voting-v1-openapi.yaml ;;
  esac
  { printf '# Mirror of github.com/lerna-labs/ekklesia-docs %s, fetched %s at %s\n' "$src" "$DATE" "$SHA"
    gh api "repos/lerna-labs/ekklesia-docs/contents/$src" --jq '.content' | base64 -d
  } > "$dst"
done
```

When refreshing, diff the result against the previous version and update
any client code that diverges from the new spec.
