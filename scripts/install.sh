#!/usr/bin/env bash
# Installs the Corpus CLI tools — `corpus` and `corpus-mcp` — into a folder you
# pick, then prints the mcpServers JSON for that exact path so you can paste it
# straight into a client such as pi or Claude Desktop.
set -euo pipefail

DEFAULT=${CORPUS_INSTALL_DIR:-/usr/local/bin}
DEFAULT=${DEFAULT/#\~/$HOME}

# Locate the binaries: next to this script (a release zip), or a repo checkout.
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
BINARY_DIR=""
for candidate in "${CORPUS_BIN_DIR:-}" "$SCRIPT_DIR" "$SCRIPT_DIR/../target/release" "$PWD/target/release"; do
  [ -n "$candidate" ] || continue
  [ -x "$candidate/corpus" ] && [ -x "$candidate/corpus-mcp" ] || continue
  BINARY_DIR=$(cd "$candidate" && pwd)
  break
done
if [ -z "$BINARY_DIR" ]; then
  echo "corpus and corpus-mcp were not found next to this script or in a nearby
target/release. Build them first (cargo build --release --workspace) or download
the CLI tools zip from the releases page and run this script from inside it." >&2
  exit 1
fi

DEST=""
if [ -n "${CORPUS_INSTALL_DIR:-}" ]; then
  DEST="$DEFAULT"
else
  printf 'Where should the Corpus CLI tools go? [%s]: ' "$DEFAULT" >&2
  # Read from the terminal so piping this script from curl cannot eat the answer.
  read -r DEST < /dev/tty 2>/dev/null || DEST=""
fi
DEST=${DEST:-$DEFAULT}
DEST="${DEST/#\~/$HOME}"
mkdir -p "$DEST"
DEST=$(cd "$DEST" && pwd)

if [ ! -w "$DEST" ]; then
  echo "Cannot write to $DEST. Re-run with sudo, or choose a folder you own,
for example: CORPUS_INSTALL_DIR=~/.local/bin $0" >&2
  exit 1
fi

cp -f "$BINARY_DIR/corpus" "$DEST/corpus"
cp -f "$BINARY_DIR/corpus-mcp" "$DEST/corpus-mcp"
chmod 755 "$DEST/corpus" "$DEST/corpus-mcp"

echo "Installed:"
echo "  $DEST/corpus        index, search, list, probe"
echo "  $DEST/corpus-mcp    stdio MCP server"

case ":$PATH:" in
  *":$DEST:"*) ;;
  *)
    echo
    echo "$DEST is not on PATH. Add it to your shell rc:"
    echo "  export PATH=\"$DEST:\$PATH\""
    ;;
esac

cat <<EOF

Connect a harness or MCP client to the corpus server — add this to the client's
mcpServers config (pi, Claude Desktop, and others that follow the convention):

{
  "mcpServers": {
    "corpus": {
      "transport": "stdio",
      "command": "$DEST/corpus-mcp",
      "args": [],
      "enabled": true,
      "timeout": 180
    }
  }
}

The two tools, search_docs and list_documents, run entirely against the local
index — no inference engine, API key or model setup needed on this side.
Index documents first (corpus index <dir> or the GUI) so the tools have
something to search.

First run of anything downloads the embedding models (~562 MB) once:
  $DEST/corpus probe
EOF
