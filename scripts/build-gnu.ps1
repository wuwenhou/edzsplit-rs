# Local build helper.
#
# The MSVC toolchain is not usable on this machine (no link.exe / partial
# Visual Studio 2019 VC layout).  The x86_64-pc-windows-gnu toolchain needs:
#   1. mingw-w64 binutils on PATH (for `as.exe` used by `dlltool`),
#   2. the rust self-contained binutils to contain that same `as.exe`.
#
# Download once from MSYS2 (7-Zip cannot open .zst; use bsdtar shipped with
# Windows or run this script which uses tar.exe):
#   $binutils = 'C:\...\binutils-<version>-any.pkg.tar.zst'
#   tar -xf $binutils -C 'C:\...\'
#   copy the mingw64\bin directory (as.exe, dlltool.exe, *.dll) into the rust
#   self-contained bin directory.
#
# Usage: pwsh scripts\build-gnu.ps1 build|test|clippy|release
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet("build", "test", "clippy", "release", "check", "fmt")]
    [string]$Command,
    [string[]]$ExtraArgs = @()
)

$ErrorActionPreference = 'Stop'
$MingwBinutils = $env:EDZSPLIT_BINUTILS
if (-not $MingwBinutils) {
    $MingwBinutils = 'C:\Users\OWG\AppData\Local\Temp\opencode\mingw-binutils\mingw64\bin'
}
$RustSelfContained = 'C:\Users\OWG\.rustup\toolchains\stable-x86_64-pc-windows-gnu\lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained'
$env:PATH = "$MingwBinutils;$RustSelfContained;$env:PATH"
$projectRoot = Split-Path -Parent $PSScriptRoot
$projectTmp = Join-Path $projectRoot 'target\tmp'
New-Item -ItemType Directory -Force $projectTmp | Out-Null
$env:TMP = $projectTmp
$env:TEMP = $projectTmp
$env:EDZSPLIT_TMP = $projectTmp

switch ($Command) {
    "build"    { cargo +stable-x86_64-pc-windows-gnu build @ExtraArgs }
    "check"    { cargo +stable-x86_64-pc-windows-gnu check @ExtraArgs }
    "test"     { cargo +stable-x86_64-pc-windows-gnu test @ExtraArgs }
    "clippy"   { cargo +stable-x86_64-pc-windows-gnu clippy --all-targets -- -D warnings @ExtraArgs }
    "release"  { cargo +stable-x86_64-pc-windows-gnu build --release @ExtraArgs }
    "fmt"      { cargo fmt --all @ExtraArgs }
}
