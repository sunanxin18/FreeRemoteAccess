#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
dist_dir="$repo_root/dist/linux"
appdir="$dist_dir/AppDir"
version="0.1.0"
artifact_prefix="FreeRemoteAccess-$version-linux-x86_64"
mkdir -p "$dist_dir" "$appdir/usr/bin" "$appdir/usr/share/applications" "$appdir/usr/share/icons/hicolor/scalable/apps" "$appdir/usr/share/metainfo"
cd "$repo_root"

cargo build --locked --release --no-default-features --features gui
cargo deb --locked --no-build --output "$dist_dir/$artifact_prefix.deb"
cargo generate-rpm
cp target/generate-rpm/*.rpm "$dist_dir/$artifact_prefix.rpm"

install -m 755 target/release/freeremotedesk "$appdir/usr/bin/freeremoteaccess"
install -m 644 packaging/linux/freeremoteaccess.desktop "$appdir/freeremoteaccess.desktop"
install -m 644 packaging/linux/freeremoteaccess.desktop "$appdir/usr/share/applications/freeremoteaccess.desktop"
install -m 644 packaging/linux/freeremoteaccess.svg "$appdir/freeremoteaccess.svg"
install -m 644 packaging/linux/freeremoteaccess.svg "$appdir/usr/share/icons/hicolor/scalable/apps/freeremoteaccess.svg"
install -m 644 packaging/linux/io.freeremote.access.metainfo.xml "$appdir/usr/share/metainfo/io.freeremote.access.metainfo.xml"
ln -sf usr/bin/freeremoteaccess "$appdir/AppRun"
curl --fail --location --retry 3 \
  --output "$dist_dir/appimagetool.AppImage" \
  https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
chmod +x "$dist_dir/appimagetool.AppImage"
ARCH=x86_64 "$dist_dir/appimagetool.AppImage" "$appdir" \
  "$dist_dir/$artifact_prefix.AppImage"
rm -rf "$appdir" "$dist_dir/appimagetool.AppImage"

for artifact in "$dist_dir/$artifact_prefix.deb" "$dist_dir/$artifact_prefix.rpm" "$dist_dir/$artifact_prefix.AppImage"; do
  digest="$(sha256sum "$artifact" | awk '{print tolower($1)}')"
  printf '%s  %s\n' "$digest" "$(basename "$artifact")" > "$artifact.sha256"
done
