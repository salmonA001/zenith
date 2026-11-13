# Sourced by the Android build scripts: where the SDK and NDK are, and the NDK's compilers and linkers
# for each target. ANDROID_HOME and ANDROID_NDK_HOME win over the guesses here.
sdk=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$LOCALAPPDATA/Android/Sdk}}
command -v cygpath >/dev/null && sdk=$(cygpath -m "$sdk")
ndk=${ANDROID_NDK_HOME:-$(ls -d "$sdk"/ndk/*/ 2>/dev/null | sort -V | tail -1)}
ndk=${ndk%/}
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) host=windows-x86_64 cmd=.cmd exe=.exe ;;
  Darwin) host=darwin-x86_64 cmd= exe= ;;
  *) host=linux-x86_64 cmd= exe= ;;
esac
bin="$ndk/toolchains/llvm/prebuilt/$host/bin"
# Windows runs the NDK's .cmd wrappers through cmd, which wants backslashes.
[ "$host" = windows-x86_64 ] && bin=$(cygpath -w "$bin") && sep='\' || sep=/
# Android 8 (API 26) is the oldest the game asks for: it is where AAudio and Vulkan 1.0 are certain.
api=26
for target in aarch64-linux-android x86_64-linux-android; do
  upper=$(echo "$target" | tr 'a-z-' 'A-Z_')
  lower=$(echo "$target" | tr '-' '_')
  # Linked by clang itself, told the target: a link line names hundreds of files, more than the
  # NDK's .cmd wrappers can pass through cmd on Windows.
  export "CARGO_TARGET_${upper}_LINKER=$bin${sep}clang$exe"
  export "CARGO_TARGET_${upper}_RUSTFLAGS=-C link-arg=--target=$target$api -C link-arg=--ld-path=$bin${sep}ld$exe"
  export "CC_${lower}=$bin$sep$target$api-clang$cmd"
  export "AR_${lower}=$bin${sep}llvm-ar$exe"
done
# Built for Windows with the GNU toolchain, some build scripts link Windows libraries by name, which
# needs dlltool. Rustup ships one with the toolchain; it is just not on the PATH.
if [ "$host" = windows-x86_64 ] && ! command -v dlltool >/dev/null; then
  self_contained="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/self-contained"
  command -v cygpath >/dev/null && self_contained=$(cygpath -u "$self_contained")
  export PATH="$self_contained:$PATH"
fi
