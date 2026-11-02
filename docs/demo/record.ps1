# Record docs/demo.gif. Needs vhs, ttyd and ffmpeg on PATH (see docs/demo/README.md).
#
# The demo runs meiose from a neutral folder (no user name in any path the
# output shows): this repo's release build and the sample config are copied to
# $DemoRoot, and removed again afterwards.
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..\..")
$DemoRoot = "C:\Users\Public\meiose-demo"

cargo build --release --quiet
Remove-Item -Recurse -Force $DemoRoot -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$DemoRoot\bin" | Out-Null
Copy-Item "target\release\meiose.exe" "$DemoRoot\bin\meiose.exe"
Copy-Item -Recurse "docs\demo\home" "$DemoRoot\home"

try {
    $env:PATH = "$DemoRoot\bin;$env:PATH"
    $env:MEIOSE_FAKE_HOME = "$DemoRoot\home"
    $env:MEIOSE_DATA_DIR = "$DemoRoot\data"
    python docs/demo/session.py
    vhs docs/demo.tape
} finally {
    Remove-Item -Recurse -Force $DemoRoot -ErrorAction SilentlyContinue
}
