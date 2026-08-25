#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
dist_dir="$repo_root/dist/macos"
work_dir="$repo_root/target/package/macos"
manifest_tool="$repo_root/packaging/package_manifest.py"
version="$(python3 "$manifest_tool" --repo "$repo_root" version)"
artifact_prefix="FreeRemoteAccess-$version-macos-universal"
app_dir="$work_dir/FreeRemoteAccess.app"
support_root="$dist_dir/THIRD_PARTY"

python3 "$repo_root/packaging/safe_cleanup.py" --repo "$repo_root" --target "$dist_dir" --expected dist/macos >/dev/null
python3 "$repo_root/packaging/safe_cleanup.py" --repo "$repo_root" --target "$work_dir" --expected target/package/macos >/dev/null
rm -rf -- "$dist_dir" "$work_dir"
mkdir -p "$dist_dir" "$work_dir" "$app_dir/Contents/MacOS" "$app_dir/Contents/Resources"
cd "$repo_root"

cargo fetch --locked
python3 "$manifest_tool" --repo "$repo_root" prepare-fdk --dest "$support_root" >/dev/null
MACOSX_DEPLOYMENT_TARGET=12.0 cargo build --locked --release --target aarch64-apple-darwin --features gui --bin freeremoteaccess-gui
MACOSX_DEPLOYMENT_TARGET=12.0 cargo build --locked --release --target x86_64-apple-darwin --features gui --bin freeremoteaccess-gui
lipo -create \
  target/aarch64-apple-darwin/release/freeremoteaccess-gui \
  target/x86_64-apple-darwin/release/freeremoteaccess-gui \
  -output "$app_dir/Contents/MacOS/FreeRemoteAccess"
chmod 755 "$app_dir/Contents/MacOS/FreeRemoteAccess"
sed "s/@PACKAGE_VERSION@/$version/g" packaging/macos/Info.plist > "$app_dir/Contents/Info.plist"
plutil -lint "$app_dir/Contents/Info.plist"
cp -R "$support_root" "$app_dir/Contents/Resources/THIRD_PARTY"

app_archive="$dist_dir/$artifact_prefix-app.zip"
dmg="$dist_dir/$artifact_prefix.dmg"
ditto -c -k --sequesterRsrc --keepParent "$app_dir" "$app_archive"
dmg_root="$work_dir/dmg-root"
mkdir -p "$dmg_root"
cp -R "$app_dir" "$dmg_root/FreeRemoteAccess.app"
hdiutil create -volname FreeRemoteAccess -srcfolder "$dmg_root" -ov -format UDZO "$dmg"

python3 "$manifest_tool" --repo "$repo_root" write \
  --dist "$dist_dir" --platform macos --arch universal \
  --support-root "$support_root" \
  --artifact "app-archive=$app_archive" \
  --artifact "dmg=$dmg"
python3 "$manifest_tool" --repo "$repo_root" verify --manifest "$dist_dir/artifact-manifest.json"
"$repo_root/packaging/macos/verify-package.sh" "$dist_dir"
