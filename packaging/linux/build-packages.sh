#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
dist_dir="$repo_root/dist/linux"
work_dir="$repo_root/target/package/linux"
appdir="$dist_dir/AppDir"
manifest_tool="$repo_root/packaging/package_manifest.py"
version="$(python3 "$manifest_tool" --repo "$repo_root" version)"
artifact_prefix="FreeRemoteAccess-$version-linux-x86_64"
support_root="$dist_dir/THIRD_PARTY"
appimagetool_url='https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage'
appimagetool_sha256='ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0'
runtime_url='https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64'
runtime_sha256='2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d'

python3 "$repo_root/packaging/safe_cleanup.py" --repo "$repo_root" --target "$dist_dir" --expected dist/linux >/dev/null
python3 "$repo_root/packaging/safe_cleanup.py" --repo "$repo_root" --target "$work_dir" --expected target/package/linux >/dev/null
rm -rf -- "$dist_dir" "$work_dir"
mkdir -p "$dist_dir" "$work_dir" "$appdir/usr/bin" \
  "$appdir/usr/share/applications" \
  "$appdir/usr/share/icons/hicolor/scalable/apps" \
  "$appdir/usr/share/metainfo" \
  "$appdir/usr/share/doc/freeremoteaccess"
cd "$repo_root"

cargo fetch --locked
python3 "$manifest_tool" --repo "$repo_root" prepare-fdk --dest "$support_root" >/dev/null
cargo build --locked --release --features gui --bin freeremoteaccess-gui
install -m 755 target/release/freeremoteaccess-gui "$appdir/usr/bin/freeremoteaccess"
install -m 644 packaging/linux/freeremoteaccess.desktop "$appdir/freeremoteaccess.desktop"
install -m 644 packaging/linux/freeremoteaccess.desktop "$appdir/usr/share/applications/freeremoteaccess.desktop"
install -m 644 packaging/linux/freeremoteaccess.svg "$appdir/freeremoteaccess.svg"
install -m 644 packaging/linux/freeremoteaccess.svg "$appdir/usr/share/icons/hicolor/scalable/apps/freeremoteaccess.svg"
install -m 644 packaging/linux/io.freeremote.access.metainfo.xml "$appdir/usr/share/metainfo/io.freeremote.access.metainfo.xml"
install -m 644 packaging/linux/runtime-libraries.txt "$appdir/usr/share/doc/freeremoteaccess/runtime-libraries.txt"
cp -a "$support_root" "$appdir/usr/share/doc/freeremoteaccess/THIRD_PARTY"
ln -s usr/bin/freeremoteaccess "$appdir/AppRun"

appdir_archive="$dist_dir/$artifact_prefix-AppDir.tar.zst"
tar --zstd -C "$dist_dir" -cf "$appdir_archive" AppDir

deb="$dist_dir/$artifact_prefix.deb"
base_deb="$work_dir/base.deb"
cargo deb --locked --no-build --output "$base_deb"
deb_root="$work_dir/deb-root"
dpkg-deb -R "$base_deb" "$deb_root"
cp -a "$appdir/usr/." "$deb_root/usr/"
dpkg-deb --build --root-owner-group "$deb_root" "$deb"

tools="$work_dir/tools"
mkdir -p "$tools"
curl --fail --location --retry 3 --output "$tools/appimagetool-1.9.1-x86_64.AppImage" "$appimagetool_url"
echo "$appimagetool_sha256  $tools/appimagetool-1.9.1-x86_64.AppImage" | sha256sum -c -
curl --fail --location --retry 3 --output "$tools/runtime-x86_64-20251108" "$runtime_url"
echo "$runtime_sha256  $tools/runtime-x86_64-20251108" | sha256sum -c -
chmod 755 "$tools/appimagetool-1.9.1-x86_64.AppImage" "$tools/runtime-x86_64-20251108"
appimage="$dist_dir/$artifact_prefix.AppImage"
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 VERSION="$version" \
  "$tools/appimagetool-1.9.1-x86_64.AppImage" \
  --runtime-file "$tools/runtime-x86_64-20251108" \
  "$appdir" "$appimage"

python3 "$manifest_tool" --repo "$repo_root" write \
  --dist "$dist_dir" --platform linux --arch x86_64 \
  --support-root "$support_root" \
  --artifact "appdir-archive=$appdir_archive" \
  --artifact "deb=$deb" \
  --artifact "appimage=$appimage" \
  --directory "appdir=$appdir"
python3 "$manifest_tool" --repo "$repo_root" verify --manifest "$dist_dir/artifact-manifest.json"
"$repo_root/packaging/linux/verify-package.sh" "$dist_dir"
