#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
dist_dir="$repo_root/dist/macos"
app_dir="$dist_dir/FreeRemoteAccess.app"
version="0.1.0"
artifact_prefix="FreeRemoteAccess-$version-macos-universal"

mkdir -p "$app_dir/Contents/MacOS" "$app_dir/Contents/Resources"
cd "$repo_root"
cargo build --locked --release --target aarch64-apple-darwin --no-default-features --features gui
cargo build --locked --release --target x86_64-apple-darwin --no-default-features --features gui
lipo -create \
  target/aarch64-apple-darwin/release/freeremotedesk \
  target/x86_64-apple-darwin/release/freeremotedesk \
  -output "$app_dir/Contents/MacOS/FreeRemoteAccess"
chmod 755 "$app_dir/Contents/MacOS/FreeRemoteAccess"
install -m 644 packaging/macos/Info.plist "$app_dir/Contents/Info.plist"

pkgbuild --root "$app_dir" \
  --install-location /Applications/FreeRemoteAccess.app \
  --identifier io.freeremote.access \
  --version 0.1.0 \
  "$dist_dir/$artifact_prefix.pkg"
hdiutil create -volname FreeRemoteAccess -srcfolder "$app_dir" -ov -format UDZO \
  "$dist_dir/$artifact_prefix.dmg"
ditto -c -k --sequesterRsrc --keepParent "$app_dir" "$dist_dir/$artifact_prefix.zip"

for artifact in "$dist_dir/$artifact_prefix.pkg" "$dist_dir/$artifact_prefix.dmg" "$dist_dir/$artifact_prefix.zip"; do
  digest="$(shasum -a 256 "$artifact" | awk '{print tolower($1)}')"
  printf '%s  %s\n' "$digest" "$(basename "$artifact")" > "$artifact.sha256"
done
