#!/bin/sh
# Cross-compile sennya.exe for 64-bit Windows from macOS or Linux.
# Needs llvm-mingw (https://github.com/mstorsjo/llvm-mingw) in $LLVM_MINGW
# (default ~/.local/share/llvm-mingw) and `rustup target add x86_64-pc-windows-gnullvm`.
# The result is target/windows/{sennya.exe, sennya.ico, install-windows.ps1}.
set -eu
cd "$(dirname "$0")/.."
LLVM_MINGW="${LLVM_MINGW:-$HOME/.local/share/llvm-mingw}"
export PATH="$LLVM_MINGW/bin:$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER=x86_64-w64-mingw32-clang
# Static runtime, so the exe needs no libunwind.dll next to it.
export CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_RUSTFLAGS="-C target-feature=+crt-static"
cargo build --release --target x86_64-pc-windows-gnullvm
mkdir -p target/windows
cp target/x86_64-pc-windows-gnullvm/release/sennya.exe target/windows/
cp assets/icon.ico target/windows/sennya.ico
cp scripts/install-windows.ps1 target/windows/
echo "target/windows/sennya.exe"
