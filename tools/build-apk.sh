#!/usr/bin/env bash
# Builds Zenith as an Android app, dist/zenith-VERSION.apk: one APK for phones (arm64-v8a) and for
# the emulator (x86_64), signed with a debug key made the first time. No Gradle: the app is all
# native code, so the SDK's own tools package it.
#
# Needs the Android SDK (build-tools 34, platform 34), the NDK, a Java runtime for keytool and
# apksigner, and Rust's Android targets:
#
#   rustup target add aarch64-linux-android x86_64-linux-android
#   tools/build-apk.sh
set -eu
cd "$(dirname "$0")/.." || exit 1
. tools/android-env.sh
build_tools=$(ls -d "$sdk"/build-tools/*/ | sort -V | tail -1)
build_tools=${build_tools%/}
platform=$(ls -d "$sdk"/platforms/android-*/ | sort -V | tail -1)
platform=${platform%/}
version=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
IFS=. read -r major minor patch <<<"$version"
code=$((major * 1000000 + minor * 1000 + patch))
keytool=${KEYTOOL:-$(command -v keytool || ls /c/Program\ Files/Java/*/bin/keytool.exe 2>/dev/null | tail -1)}
work=target/apk
rm -rf "$work"
mkdir -p "$work/lib" "$work/res" dist

# The game, once for each kind of processor, without its symbol table (a third of the file).
for pair in arm64-v8a:aarch64-linux-android x86_64:x86_64-linux-android; do
  abi=${pair%%:*}
  target=${pair#*:}
  cargo build --release --target "$target" -p zenith-android
  mkdir -p "$work/lib/$abi"
  "$bin/llvm-strip" --strip-all -o "$work/lib/$abi/libzenith_android.so" "target/$target/release/libzenith_android.so"
done

# The launcher icon at each screen density, painted by the same code as the game's own.
for density in mdpi:48 hdpi:72 xhdpi:96 xxhdpi:144 xxxhdpi:192; do
  mkdir -p "$work/res/mipmap-${density%%:*}"
  cargo run -q --release -p zenith-client --example icon -- "${density#*:}" "$work/res/mipmap-${density%%:*}/ic_launcher.png"
done

"$build_tools/aapt2" compile --dir "$work/res" -o "$work/res.zip"
"$build_tools/aapt2" link -o "$work/resources.apk" -I "$platform/android.jar" \
  --manifest tools/android/AndroidManifest.xml -R "$work/res.zip" --auto-add-overlay \
  --version-name "$version" --version-code "$code"

# The libraries go in uncompressed, so Android maps them straight from the APK. That needs each to
# start on a page boundary: 16 KB, for the phones with 16 KB pages. This writes the alignment the
# way zipalign does, in the extra field Android keeps for it, so apksigner keeps it too.
python -I - "$work/resources.apk" "$work/lib" "$work/aligned.apk" <<'PY'
import os, struct, sys, zipfile
resources, libs, out = sys.argv[1:]

def align(info, offset, multiple):
    start = offset + 30 + len(info.filename.encode()) + 6
    padding = -start % multiple
    info.extra = struct.pack('<HHH', 0xD935, 2 + padding, multiple) + bytes(padding)

with zipfile.ZipFile(resources) as old, zipfile.ZipFile(out, 'w') as apk:
    def add(name, data, compression, multiple, when=(1980, 1, 1, 0, 0, 0)):
        info = zipfile.ZipInfo(name, when)
        info.compress_type = compression
        if compression == zipfile.ZIP_STORED:
            align(info, apk.fp.tell(), multiple)
        apk.writestr(info, data)
    for info in old.infolist():
        add(info.filename, old.read(info), info.compress_type, 4, info.date_time)
    for abi in sorted(os.listdir(libs)):
        for name in sorted(os.listdir(os.path.join(libs, abi))):
            with open(os.path.join(libs, abi, name), 'rb') as f:
                add(f'lib/{abi}/{name}', f.read(), zipfile.ZIP_STORED, 16384)
PY

key=target/apk-debug.keystore
if [ ! -f "$key" ]; then
  "$keytool" -genkeypair -storetype PKCS12 -keystore "$key" -storepass android -keypass android -alias zenith \
    -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Zenith debug build" >/dev/null
fi
apk="dist/zenith-$version.apk"
apksigner=$build_tools/apksigner
[ -f "$apksigner.bat" ] && apksigner=$apksigner.bat
"$apksigner" sign --v4-signing-enabled false --ks "$key" --ks-pass pass:android --key-pass pass:android --out "$apk" "$work/aligned.apk"
"$apksigner" verify "$apk"
"$build_tools/zipalign" -c -p 4 "$apk"
python -I - "$apk" <<'PY'
import struct, sys, zipfile
with open(sys.argv[1], 'rb') as f, zipfile.ZipFile(f) as apk:
    for info in apk.infolist():
        if info.filename.endswith('.so'):
            f.seek(info.header_offset)
            name, extra = struct.unpack('<26xHH', f.read(30))
            start = info.header_offset + 30 + name + extra
            if info.compress_type != zipfile.ZIP_STORED or start % 16384:
                sys.exit(f'{info.filename} is not stored on a 16 KB boundary')
PY
echo "$apk: $(du -h "$apk" | cut -f1)"
