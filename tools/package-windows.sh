#!/usr/bin/env bash
# Builds Zenith for Windows and packs it to share, as dist/zenith-VERSION-windows-x64.zip:
#
#   zenith.exe          the game (no console window; it shows errors in a message box)
#   zenith-server.exe   a dedicated server
#   zenith-ctl.exe      plays and controls the game from the command line
#   mc-modkit.exe       installs Fabric, Quilt, Forge or NeoForge and mods for Minecraft
#
# with the README, the licences and the docs. Nothing else is needed to run it: the programs use only
# what comes with Windows, and the graphics driver.
#
#   tools/package-windows.sh
set -eu
cd "$(dirname "$0")/.." || exit 1
version=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
name="zenith-$version-windows-x64"
cargo build --release -p zenith-client -p zenith-server -p zenith-ctl -p mc-modkit
rm -rf "dist/$name" "dist/$name.zip"
mkdir -p "dist/$name"
for program in zenith zenith-server zenith-ctl mc-modkit; do
  cp "target/release/$program.exe" "dist/$name/"
done
cp README.md LICENSE NOTICE.md "dist/$name/"
cp -r docs "dist/$name/docs"
(cd dist && python -I -m zipfile -c "$name.zip" "$name")
echo "dist/$name.zip: $(du -h "dist/$name.zip" | cut -f1)"
