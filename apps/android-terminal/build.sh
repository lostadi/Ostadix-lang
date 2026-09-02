#!/data/data/com.termux/files/usr/bin/bash
set -euo pipefail

APP_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$APP_ROOT/../.." && pwd)
BUILD_ROOT="$APP_ROOT/build"
INTERMEDIATES="$BUILD_ROOT/intermediates"
OUTPUT_DIR="$BUILD_ROOT/outputs/apk/debug"
ANDROID_SDK_ROOT=${ANDROID_SDK_ROOT:-$HOME/android-sdk}
ANDROID_JAR=${ANDROID_JAR:-$ANDROID_SDK_ROOT/platforms/android-34/android.jar}
DEBUG_KEYSTORE=${DEBUG_KEYSTORE:-$HOME/.android/debug.keystore}
TERMUX_PREFIX=${TERMUX_PREFIX:-/data/data/com.termux/files/usr}
TERMUX_LIB="$TERMUX_PREFIX/lib"
TERMUX_DOC="$TERMUX_PREFIX/share/doc"
BUNDLED_BASH_INPUT="$TERMUX_PREFIX/bin/bash"
ANDROID_BUILD_JOBS=${OSTADIX_ANDROID_BUILD_JOBS:-6}
ANDROID_CPU_PROFILE=${OSTADIX_ANDROID_CPU_PROFILE:-portable}
case "$ANDROID_CPU_PROFILE" in
    portable)
        RUST_TARGET_CPU=generic
        PTY_CPU_FLAG=-march=armv8-a
        APK_NAME_SUFFIX=
        ;;
    native)
        RUST_TARGET_CPU=native
        PTY_CPU_FLAG=-mcpu=native
        APK_NAME_SUFFIX=-device-native
        ;;
    *)
        echo "OSTADIX_ANDROID_CPU_PROFILE must be portable or native" >&2
        exit 1
        ;;
esac
ANDROID_RUSTFLAGS="-C target-cpu=$RUST_TARGET_CPU -C linker=clang -C link-arg=-fuse-ld=lld -C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384"
SCCACHE_IDLE_TIMEOUT_SECONDS=600

require_tool() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "Missing required build tool: $1" >&2
        exit 1
    }
}

for tool in aapt2 apksigner cargo clang d8 env jar java javac keytool ld.lld python3 readelf sha256sum; do
    require_tool "$tool"
done

if [[ ! "$ANDROID_BUILD_JOBS" =~ ^[1-9][0-9]*$ ]]; then
    echo "OSTADIX_ANDROID_BUILD_JOBS must be a positive integer" >&2
    exit 1
fi

RUST_BUILD_ENV=(
    env
    --unset=CARGO_ENCODED_RUSTFLAGS
    --unset=RUSTC_WORKSPACE_WRAPPER
    --unset=RUSTC_WRAPPER
    "CARGO_BUILD_JOBS=$ANDROID_BUILD_JOBS"
    "CARGO_INCREMENTAL=0"
    "RUSTFLAGS=$ANDROID_RUSTFLAGS"
)
if SCCACHE_PATH=$(command -v sccache 2>/dev/null); then
    RUST_BUILD_ENV+=(
        "RUSTC_WRAPPER=$SCCACHE_PATH"
        "SCCACHE_IDLE_TIMEOUT=$SCCACHE_IDLE_TIMEOUT_SECONDS"
    )
    RUST_CACHE_DESCRIPTION="sccache $SCCACHE_PATH (${SCCACHE_IDLE_TIMEOUT_SECONDS}s idle timeout)"
else
    SCCACHE_PATH=""
    RUST_CACHE_DESCRIPTION="disabled (sccache is not installed)"
fi

PTY_COMPILER=(clang)
if CCACHE_PATH=$(command -v ccache 2>/dev/null); then
    PTY_COMPILER=("$CCACHE_PATH" clang)
    C_CACHE_DESCRIPTION="ccache $CCACHE_PATH"
else
    CCACHE_PATH=""
    C_CACHE_DESCRIPTION="disabled (ccache is not installed)"
fi

if [[ ! -f "$ANDROID_JAR" ]]; then
    echo "Android platform jar not found: $ANDROID_JAR" >&2
    exit 1
fi

if [[ -d "$INTERMEDIATES" ]]; then
    find "$INTERMEDIATES" -mindepth 1 -delete
fi
mkdir -p \
    "$INTERMEDIATES/compiled-res" \
    "$INTERMEDIATES/generated" \
    "$INTERMEDIATES/classes" \
    "$INTERMEDIATES/test-classes" \
    "$INTERMEDIATES/dex" \
    "$INTERMEDIATES/package/lib/arm64-v8a" \
    "$INTERMEDIATES/assets/backends" \
    "$INTERMEDIATES/assets/licenses" \
    "$INTERMEDIATES/assets/terminfo" \
    "$INTERMEDIATES/cli-smoke/bin" \
    "$INTERMEDIATES/cli-smoke/home" \
    "$INTERMEDIATES/cli-smoke/tmp" \
    "$OUTPUT_DIR"

echo "CPU profile:   $ANDROID_CPU_PROFILE (Rust $RUST_TARGET_CPU, PTY $PTY_CPU_FLAG)"
echo "Native linker: Clang/LLD, 16 KiB ELF pages"
echo "Rust cache:    $RUST_CACHE_DESCRIPTION"
echo "C cache:       $C_CACHE_DESCRIPTION"

echo "[1/10] Building the in-process Ostadix Android runtime"
"${RUST_BUILD_ENV[@]}" cargo build \
        --release \
        --locked \
        --manifest-path "$APP_ROOT/runtime/Cargo.toml"
cp "$APP_ROOT/runtime/target/release/libostadix_runtime.so" \
    "$INTERMEDIATES/package/lib/arm64-v8a/libostadix_runtime.so"

echo "[2/10] Building the standalone O CLI"
"${RUST_BUILD_ENV[@]}" cargo build \
        --release \
        --locked \
        --manifest-path "$REPO_ROOT/Cargo.toml" \
        --bin O
cp "$REPO_ROOT/target/release/O" \
    "$INTERMEDIATES/package/lib/arm64-v8a/libostadix_cli.so"

echo "[3/10] Staging the standalone GNU Bash runtime"
READLINE_SOURCE=$(readlink -f "$TERMUX_LIB/libreadline.so.8")
NCURSESW_SOURCE=$(readlink -f "$TERMUX_LIB/libncursesw.so.6")
declare -A BUNDLED_BASH_INPUT_HASHES=(
    ["$BUNDLED_BASH_INPUT"]="0179c7b15fb3df857608ef745daa17077523ffc42b7755ccc725ad7a712698a2"
    ["$TERMUX_LIB/libandroid-support.so"]="739cf829511d71dafd6c67fdbb70f3f0c6048642ea2e1967790ee961fde14430"
    ["$TERMUX_LIB/libiconv.so"]="53349c7a84ad06da53c3976754c742d6a79f3297562f0fbe61b7ee620f783667"
    ["$READLINE_SOURCE"]="aab81ed5d196100e7b2c2a7606b2cba2cffef2395c3ef3e602dca804f9c6acba"
    ["$NCURSESW_SOURCE"]="795f855f5a988d9e89116847b2c9aa03720cedbc02026259ca735be25398c4c5"
)
for bash_input in "${!BUNDLED_BASH_INPUT_HASHES[@]}"; do
    if [[ ! -f "$bash_input" ]]; then
        echo "Missing pinned Bash runtime input: $bash_input" >&2
        exit 1
    fi
    actual_hash=$(sha256sum "$bash_input")
    actual_hash=${actual_hash%% *}
    if [[ "$actual_hash" != "${BUNDLED_BASH_INPUT_HASHES[$bash_input]}" ]]; then
        echo "Bash runtime input changed; update source attribution and hashes: $bash_input" >&2
        echo "  expected ${BUNDLED_BASH_INPUT_HASHES[$bash_input]}" >&2
        echo "  actual   $actual_hash" >&2
        exit 1
    fi
done

BASH_PACKAGE_DIR="$INTERMEDIATES/package/lib/arm64-v8a"
cp "$BUNDLED_BASH_INPUT" "$BASH_PACKAGE_DIR/libostadix_bash.so"
cp "$TERMUX_LIB/libandroid-support.so" "$BASH_PACKAGE_DIR/libandroid-support.so"
cp "$TERMUX_LIB/libiconv.so" "$BASH_PACKAGE_DIR/libiconv.so"
cp "$READLINE_SOURCE" "$BASH_PACKAGE_DIR/libreadline_8.so"
cp "$NCURSESW_SOURCE" "$BASH_PACKAGE_DIR/libncursesw_6.so"

# Android supports ${ORIGIN} RUNPATH from API 24. Keep it on this private
# dependency closure, while a child-only LD_LIBRARY_PATH remains the tested
# fallback for linker namespaces that ignore a main executable's RUNPATH.
python3 "$APP_ROOT/tools/scrub_elf_runpath.py" \
    --set-runpath '${ORIGIN}' \
    --replace-needed libreadline.so.8=libreadline_8.so \
    "$BASH_PACKAGE_DIR/libostadix_bash.so"
python3 "$APP_ROOT/tools/scrub_elf_runpath.py" \
    --set-runpath '${ORIGIN}' \
    --replace-needed libncursesw.so.6=libncursesw_6.so \
    --replace-soname libreadline.so.8=libreadline_8.so \
    "$BASH_PACKAGE_DIR/libreadline_8.so"
python3 "$APP_ROOT/tools/scrub_elf_runpath.py" \
    --set-runpath '${ORIGIN}' \
    --replace-soname libncursesw.so.6=libncursesw_6.so \
    "$BASH_PACKAGE_DIR/libncursesw_6.so"
python3 "$APP_ROOT/tools/scrub_elf_runpath.py" \
    --set-runpath '${ORIGIN}' \
    "$BASH_PACKAGE_DIR/libandroid-support.so" \
    "$BASH_PACKAGE_DIR/libiconv.so"

echo "[4/10] Building the PTY JNI bridge"
JAVA_ROOT=$(dirname "$(dirname "$(readlink -f "$(command -v javac)")")")
PTY_OBJECT="$INTERMEDIATES/ostadix_pty.o"
CCACHE_BASEDIR="$REPO_ROOT" \
CCACHE_COMPILERCHECK=content \
CCACHE_NOHASHDIR=true \
"${PTY_COMPILER[@]}" \
    -c -fPIC -O3 "$PTY_CPU_FLAG" -flto=thin \
    -ffunction-sections -fdata-sections \
    -fvisibility=hidden -fstack-protector-strong \
    -D_FORTIFY_SOURCE=2 \
    -I"$JAVA_ROOT/include" -I"$JAVA_ROOT/include/linux" \
    "$APP_ROOT/app/src/main/cpp/ostadix_pty.c" \
    -o "$PTY_OBJECT"
clang \
    -shared -O3 "$PTY_CPU_FLAG" -flto=thin -fuse-ld=lld \
    -Wl,-soname,libostadix_pty.so \
    -Wl,-z,relro,-z,now,-z,noexecstack \
    -Wl,--gc-sections \
    -Wl,-z,max-page-size=16384 -Wl,-z,common-page-size=16384 \
    "$PTY_OBJECT" \
    -o "$INTERMEDIATES/package/lib/arm64-v8a/libostadix_pty.so"

for native_object in "$INTERMEDIATES/package/lib/arm64-v8a/"*.so; do
    case "$(basename "$native_object")" in
        libostadix_bash.so|libandroid-support.so|libiconv.so|libreadline_8.so|libncursesw_6.so)
            path_tag_count=$(readelf -d "$native_object" \
                | grep -Ec '\((RPATH|RUNPATH)\)' || true)
            if [[ "$path_tag_count" != 1 ]] \
                    || ! readelf -d "$native_object" \
                        | grep -Fq 'Library runpath: [${ORIGIN}]'; then
                echo "Bash runtime object lacks exact \${ORIGIN} RUNPATH: $native_object" >&2
                exit 1
            fi
            ;;
        *)
            python3 "$APP_ROOT/tools/scrub_elf_runpath.py" "$native_object"
            if readelf -d "$native_object" | grep -Eq '\((RPATH|RUNPATH)\)'; then
                echo "Host RPATH/RUNPATH survived scrubbing: $native_object" >&2
                exit 1
            fi
            ;;
    esac
    if readelf -d "$native_object" | grep -Fq '/data/data/com.termux'; then
        echo "Host Termux path survived native dynamic metadata cleanup: $native_object" >&2
        exit 1
    fi
    if readelf -d "$native_object" | grep -q '(TEXTREL)'; then
        echo "Native object contains forbidden text relocations: $native_object" >&2
        exit 1
    fi
    if ! readelf -lW "$native_object" | awk '/LOAD/{ if ($NF != "0x4000") bad=1 } END{ exit bad }'; then
        echo "Native object is not 16 KiB page aligned: $native_object" >&2
        exit 1
    fi
done
if ! readelf -lW "$INTERMEDIATES/package/lib/arm64-v8a/libostadix_cli.so" \
        | awk '/\/system\/bin\/linker64/{found=1} END{exit !found}'; then
    echo "Bundled O CLI is not an Android ARM64 PIE executable" >&2
    exit 1
fi

if ! readelf -lW "$BASH_PACKAGE_DIR/libostadix_bash.so" \
        | awk '/\/system\/bin\/linker64/{found=1} END{exit !found}'; then
    echo "Bundled Bash is not an Android ARM64 PIE executable" >&2
    exit 1
fi
if ! readelf -d "$BASH_PACKAGE_DIR/libreadline_8.so" \
        | grep -Fq 'Library soname: [libreadline_8.so]'; then
    echo "Bundled Readline SONAME was not normalized" >&2
    exit 1
fi
if ! readelf -d "$BASH_PACKAGE_DIR/libncursesw_6.so" \
        | grep -Fq 'Library soname: [libncursesw_6.so]'; then
    echo "Bundled ncurses SONAME was not normalized" >&2
    exit 1
fi
for bash_object in \
        "$BASH_PACKAGE_DIR/libostadix_bash.so" \
        "$BASH_PACKAGE_DIR/libandroid-support.so" \
        "$BASH_PACKAGE_DIR/libiconv.so" \
        "$BASH_PACKAGE_DIR/libreadline_8.so" \
        "$BASH_PACKAGE_DIR/libncursesw_6.so"; do
    while IFS= read -r dependency; do
        case "$dependency" in
            libc.so|libdl.so)
                ;;
            *.so)
                dependency_matches=$(find "$BASH_PACKAGE_DIR" \
                    -maxdepth 1 -type f -name "$dependency" -print | wc -l)
                if [[ "$dependency_matches" != 1 ]]; then
                    echo "Private Bash dependency does not map exactly once: " \
                        "$dependency <- $bash_object" >&2
                    exit 1
                fi
                ;;
            *)
                echo "Bash dependency is not APK-extractable: $dependency" >&2
                exit 1
                ;;
        esac
    done < <(readelf -d "$bash_object" \
        | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p')
done

echo "[5/10] Staging bundled assets, licenses, and terminal data"
cp -R "$APP_ROOT/app/src/main/assets/." "$INTERMEDIATES/assets/"
find "$REPO_ROOT/backends" -maxdepth 1 -type f -name '*.py' -exec cp '{}' "$INTERMEDIATES/assets/backends/" ';'
cp "$REPO_ROOT/LICENSE" "$INTERMEDIATES/assets/licenses/Ostadix-LICENSE.txt"
cp "$REPO_ROOT/NOTICE" "$INTERMEDIATES/assets/licenses/Ostadix-NOTICE.txt"
cp -R "$TERMUX_PREFIX/share/terminfo/." "$INTERMEDIATES/assets/terminfo/"
cp -L "$TERMUX_DOC/bash/copyright" \
    "$INTERMEDIATES/assets/licenses/GNU-Bash-GPL-3.0.txt"
cp -L "$TERMUX_DOC/readline/copyright" \
    "$INTERMEDIATES/assets/licenses/GNU-Readline-GPL-3.0.txt"
cp -L "$TERMUX_DOC/libiconv/copyright" \
    "$INTERMEDIATES/assets/licenses/GNU-libiconv-LGPL-2.1.txt"
cp -L "$TERMUX_DOC/libiconv/copyright.1" \
    "$INTERMEDIATES/assets/licenses/GNU-libiconv-GPL-3.0.txt"
cp -L "$TERMUX_DOC/ncurses/copyright" \
    "$INTERMEDIATES/assets/licenses/ncurses-LICENSE.txt"
cp -L "$TERMUX_DOC/libandroid-support/LICENSE.txt" \
    "$INTERMEDIATES/assets/licenses/libandroid-support-LICENSE.txt"
cp -L "$TERMUX_DOC/libandroid-support/LICENSE.txt.1" \
    "$INTERMEDIATES/assets/licenses/libandroid-support-LICENSE-2.txt"
if [[ ! -s "$INTERMEDIATES/assets/terminfo/x/xterm-256color" ]]; then
    echo "Bundled xterm-256color terminfo entry is missing" >&2
    exit 1
fi

echo "[6/10] Smoke-testing standalone Bash and O commands"
CLI_SMOKE_ROOT="$INTERMEDIATES/cli-smoke"
ln -s "$INTERMEDIATES/package/lib/arm64-v8a/libostadix_cli.so" \
    "$CLI_SMOKE_ROOT/bin/O"
ln -s "$BASH_PACKAGE_DIR/libostadix_bash.so" \
    "$CLI_SMOKE_ROOT/bin/bash"
CLI_SMOKE_ENV=(
    env -i
    "HOME=$CLI_SMOKE_ROOT/home"
    "TMPDIR=$CLI_SMOKE_ROOT/tmp"
    "PATH=$CLI_SMOKE_ROOT/bin:/system/bin:/system/xbin"
    "SHELL=$CLI_SMOKE_ROOT/bin/bash"
    "LD_LIBRARY_PATH=$BASH_PACKAGE_DIR"
    "TERM=xterm-256color"
    "TERMINFO=$INTERMEDIATES/assets/terminfo"
    "INPUTRC=$INTERMEDIATES/assets/shell/inputrc"
    "LANG=C.UTF-8"
    "ANDROID_ROOT=/system"
    "ANDROID_DATA=/data"
    "O_BACKENDS_DIR=$INTERMEDIATES/assets/backends"
)
BASH_RESOLVED=$("${CLI_SMOKE_ENV[@]}" /system/bin/sh -c 'command -v bash')
if [[ "$BASH_RESOLVED" != "$CLI_SMOKE_ROOT/bin/bash" ]]; then
    echo "Standalone shell could not discover Bash: $BASH_RESOLVED" >&2
    exit 1
fi
BUNDLED_BASH_VERSION_LINE=$("${CLI_SMOKE_ENV[@]}" /system/bin/sh -c 'bash --version' \
    | sed -n '1p')
if [[ "$BUNDLED_BASH_VERSION_LINE" != "GNU bash, version 5.3.15(1)-release (aarch64-unknown-linux-android)" ]]; then
    echo "Standalone bash --version smoke test failed: $BUNDLED_BASH_VERSION_LINE" >&2
    exit 1
fi
BUNDLED_BASH_FEATURE_RESULT=$("${CLI_SMOKE_ENV[@]}" /system/bin/sh -c \
    "bash --noprofile --norc -c 'a=(zero one); [[ \"\${a[1]}\" == one ]]; "\
"brace=\$(printf \"%s\" {a,b,c}); "\
"pipe=\$(printf pipeline | { IFS= read -r value; printf \"%s\" \"\$value\"; }); "\
"IFS= read -r process < <(printf substitution); "\
"printf \"%s:%s:%s:%s\" \"\${a[1]}\" \"\$brace\" \"\$pipe\" \"\$process\"'")
if [[ "$BUNDLED_BASH_FEATURE_RESULT" != "one:abc:pipeline:substitution" ]]; then
    echo "Standalone Bash feature smoke test failed: $BUNDLED_BASH_FEATURE_RESULT" >&2
    exit 1
fi
# Retain the requested login-shell check as a command/ABI smoke. The stronger
# test above bypasses the build host's Termux profiles and proves Bash syntax.
BUNDLED_BASH_LOGIN_RESULT=$("${CLI_SMOKE_ENV[@]}" /system/bin/sh -c \
    "bash -lc 'printf standalone-bash-login-ok'")
if [[ "$BUNDLED_BASH_LOGIN_RESULT" != "standalone-bash-login-ok" ]]; then
    echo "Standalone bash -lc smoke test failed: $BUNDLED_BASH_LOGIN_RESULT" >&2
    exit 1
fi
CLI_RESOLVED=$("${CLI_SMOKE_ENV[@]}" /system/bin/sh -c 'command -v O')
if [[ "$CLI_RESOLVED" != "$CLI_SMOKE_ROOT/bin/O" ]]; then
    echo "Standalone shell could not discover O: $CLI_RESOLVED" >&2
    exit 1
fi
CLI_HELP=$("${CLI_SMOKE_ENV[@]}" /system/bin/sh -c 'O --help')
if [[ "$CLI_HELP" != Usage:* ]]; then
    echo "Standalone O --help smoke test failed" >&2
    exit 1
fi
CLI_EVAL=$("${CLI_SMOKE_ENV[@]}" /system/bin/sh -c "O --eval '2'")
if [[ "$CLI_EVAL" != "2" ]]; then
    echo "Standalone O --eval smoke test failed: $CLI_EVAL" >&2
    exit 1
fi
echo "  command -v bash -> $BASH_RESOLVED"
echo "  bash --version -> $BUNDLED_BASH_VERSION_LINE"
echo "  Bash language features -> $BUNDLED_BASH_FEATURE_RESULT"
echo "  bash -lc -> $BUNDLED_BASH_LOGIN_RESULT"
echo "  command -v O -> $CLI_RESOLVED"
echo "  O --eval 2 -> $CLI_EVAL"

echo "[7/10] Compiling Android resources"
aapt2 compile \
    --dir "$APP_ROOT/app/src/main/res" \
    -o "$INTERMEDIATES/compiled-res/resources.zip"
aapt2 link \
    -o "$INTERMEDIATES/base-unsigned.apk" \
    -I "$ANDROID_JAR" \
    --manifest "$APP_ROOT/app/src/main/AndroidManifest.xml" \
    --java "$INTERMEDIATES/generated" \
    --min-sdk-version 28 \
    --target-sdk-version 34 \
    --version-code 7 \
    --version-name 0.1.6 \
    -A "$INTERMEDIATES/assets" \
    -R "$INTERMEDIATES/compiled-res/resources.zip" \
    --auto-add-overlay

echo "[8/10] Compiling Java and DEX bytecode"
python3 "$APP_ROOT/tools/verify_root_environment.py" \
    "$APP_ROOT/app/src/main/java/org/ostadix/terminal/AppFiles.java" \
    "$APP_ROOT/app/src/main/java/org/ostadix/terminal/MainActivity.java"
mapfile -t JAVA_SOURCES < <(find \
    "$APP_ROOT/app/src/main/java" \
    "$INTERMEDIATES/generated" \
    -type f -name '*.java' -print | sort)
javac \
    -encoding UTF-8 \
    -source 8 -target 8 \
    -bootclasspath "$ANDROID_JAR" \
    -classpath "$ANDROID_JAR" \
    -d "$INTERMEDIATES/classes" \
    "${JAVA_SOURCES[@]}"
javac \
    -encoding UTF-8 \
    -source 8 -target 8 \
    -bootclasspath "$ANDROID_JAR" \
    -classpath "$ANDROID_JAR:$INTERMEDIATES/classes" \
    -d "$INTERMEDIATES/test-classes" \
    "$APP_ROOT/app/src/test/java/org/ostadix/terminal/TerminalCoreSelfTest.java" \
    "$APP_ROOT/app/src/test/java/org/ostadix/terminal/PtyJniSmoke.java" \
    "$APP_ROOT/app/src/test/java/org/ostadix/terminal/RuntimeJniSmoke.java"
java \
    -classpath "$INTERMEDIATES/test-classes:$INTERMEDIATES/classes:$ANDROID_JAR" \
    org.ostadix.terminal.TerminalCoreSelfTest
JNI_LIBRARY_DIR="$INTERMEDIATES/package/lib/arm64-v8a"
env "LD_LIBRARY_PATH=$JNI_LIBRARY_DIR:$TERMUX_LIB" \
    java -Djava.library.path="$JNI_LIBRARY_DIR" \
    -classpath "$INTERMEDIATES/test-classes:$INTERMEDIATES/classes:$ANDROID_JAR" \
    org.ostadix.terminal.PtyJniSmoke
env "LD_LIBRARY_PATH=$JNI_LIBRARY_DIR:$TERMUX_LIB" \
    java -Djava.library.path="$JNI_LIBRARY_DIR" \
    -classpath "$INTERMEDIATES/test-classes:$INTERMEDIATES/classes:$ANDROID_JAR" \
    org.ostadix.terminal.RuntimeJniSmoke "$INTERMEDIATES/assets/backends"
jar cf "$INTERMEDIATES/classes.jar" -C "$INTERMEDIATES/classes" .
d8 \
    --min-api 28 \
    --lib "$ANDROID_JAR" \
    --output "$INTERMEDIATES/dex" \
    "$INTERMEDIATES/classes.jar"

echo "[9/10] Packaging and signing the standalone APK"
cp "$INTERMEDIATES/base-unsigned.apk" "$INTERMEDIATES/OstadixTerminal-unsigned.apk"
jar uf "$INTERMEDIATES/OstadixTerminal-unsigned.apk" \
    -C "$INTERMEDIATES/dex" classes.dex \
    -C "$INTERMEDIATES/package" lib
for apk_entry in \
        lib/arm64-v8a/libostadix_cli.so \
        lib/arm64-v8a/libostadix_bash.so \
        lib/arm64-v8a/libandroid-support.so \
        lib/arm64-v8a/libiconv.so \
        lib/arm64-v8a/libreadline_8.so \
        lib/arm64-v8a/libncursesw_6.so \
        assets/licenses/Bundled-Bash-SOURCES.txt \
        assets/licenses/GNU-Bash-GPL-3.0.txt \
        assets/licenses/GNU-Readline-GPL-3.0.txt \
        assets/licenses/GNU-libiconv-LGPL-2.1.txt \
        assets/licenses/GNU-libiconv-GPL-3.0.txt \
        assets/licenses/ncurses-LICENSE.txt \
        assets/licenses/libandroid-support-LICENSE.txt \
        assets/licenses/libandroid-support-LICENSE-2.txt \
        assets/shell/inputrc \
        assets/terminfo/x/xterm-256color; do
    if ! jar tf "$INTERMEDIATES/OstadixTerminal-unsigned.apk" \
            | awk -v expected="$apk_entry" \
                '$0 == expected{found=1} END{exit !found}'; then
        echo "Unsigned APK is missing required entry: $apk_entry" >&2
        exit 1
    fi
done

if [[ ! -f "$DEBUG_KEYSTORE" ]]; then
    mkdir -p "$(dirname "$DEBUG_KEYSTORE")"
    keytool -genkeypair -noprompt \
        -keystore "$DEBUG_KEYSTORE" \
        -storepass android \
        -alias androiddebugkey \
        -keypass android \
        -dname 'CN=Android Debug,O=Android,C=US' \
        -keyalg RSA -keysize 2048 -validity 10000
fi

APK="$OUTPUT_DIR/OstadixTerminal${APK_NAME_SUFFIX}-debug.apk"
UNIVERSAL_APK="$OUTPUT_DIR/OstadixTerminal${APK_NAME_SUFFIX}-universal.apk"
apksigner sign \
    --ks "$DEBUG_KEYSTORE" \
    --ks-key-alias androiddebugkey \
    --ks-pass pass:android \
    --key-pass pass:android \
    --min-sdk-version 28 \
    --v1-signing-enabled true \
    --v2-signing-enabled true \
    --v3-signing-enabled true \
    --v4-signing-enabled false \
    --alignment-preserved false \
    --lib-page-alignment 16384 \
    --out "$APK" \
    "$INTERMEDIATES/OstadixTerminal-unsigned.apk"

# Some third-party installers misclassify APKs whose newest signature is v3.
# Keep a v2-only compatibility artifact for Android 9+ installer front ends.
apksigner sign \
    --ks "$DEBUG_KEYSTORE" \
    --ks-key-alias androiddebugkey \
    --ks-pass pass:android \
    --key-pass pass:android \
    --min-sdk-version 28 \
    --v1-signing-enabled true \
    --v2-signing-enabled true \
    --v3-signing-enabled false \
    --v4-signing-enabled false \
    --alignment-preserved false \
    --lib-page-alignment 16384 \
    --out "$UNIVERSAL_APK" \
    "$INTERMEDIATES/OstadixTerminal-unsigned.apk"

echo "[10/10] Verifying the APK"
apksigner verify --verbose --print-certs "$APK"
apksigner verify --verbose --print-certs "$UNIVERSAL_APK"
aapt2 dump badging "$APK" | sed -n '1,8p'
ls -lh "$APK" "$UNIVERSAL_APK"
echo "$APK"
echo "$UNIVERSAL_APK"
