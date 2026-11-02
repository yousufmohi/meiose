# Record docs/demo.gif. Needs vhs, ttyd and ffmpeg on PATH (see docs/demo/README.md).
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..\..")
$env:MEIOSE_DATA_DIR = (Resolve-Path "docs/demo").Path + "\.data"
Remove-Item -Recurse -Force $env:MEIOSE_DATA_DIR -ErrorAction SilentlyContinue
python docs/demo/session.py
vhs docs/demo.tape
