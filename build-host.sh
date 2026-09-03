#!/bin/bash
# Build site-tools for the host it publishes from: mail.lindfors.no, Alpine on aarch64,
# musl libc. Same arrangement as newsletter/build.sh, and for the same reason: without
# the `cite` feature the binary carries no C, so a rustup target and rust-lld are the
# whole toolchain (see .cargo/config.toml).
#
# What the box needs from this binary is markdown, speech, pdf, og, newsletter gen and
# publish. Citations are resolved on the workstation before a post is queued, and
# `site-tools schedule` refuses a post that still has a marker.

set -e

cd "$(dirname "${BASH_SOURCE[0]}")"

TARGET="aarch64-unknown-linux-musl"
BIN="target/$TARGET/release/site-tools"

if ! rustup target list --installed | grep -qx "$TARGET"; then
    echo "Adding $TARGET..."
    rustup target add "$TARGET"
fi

echo "Building for $TARGET without the cite feature..."
cargo build --release --target "$TARGET" --no-default-features

echo
echo "Built: $BIN"
ls -lh "$BIN" | awk '{print "  size: " $5}'
file "$BIN" 2>/dev/null | sed 's/^/  /' || true

cat <<'EOF'

Copy it over and install it (host/README.md has the whole first-time setup):

  scp tools/site-tools/target/aarch64-unknown-linux-musl/release/site-tools hetzner:/tmp/
  ssh hetzner 'sudo install -m 755 /tmp/site-tools /opt/lindfors-publisher/site-tools'
EOF
