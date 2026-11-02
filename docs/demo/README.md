# Recording the demo GIF

The GIF is rendered by [VHS](https://github.com/charmbracelet/vhs) from
`docs/demo.tape`. Everything it shows is real output: `record.ps1` first runs a
real Playwright session through meiose (`session.py`) into a throwaway data
directory, then VHS types the commands.

1. Install VHS and its two helpers, ttyd and ffmpeg, and put them on PATH:
   - VHS: https://github.com/charmbracelet/vhs/releases (`vhs_*_Windows_x86_64.zip`)
   - ttyd: https://github.com/tsl0922/ttyd/releases (`ttyd.win32.exe`, rename to `ttyd.exe`)
   - ffmpeg: https://github.com/BtbN/FFmpeg-Builds/releases (`ffmpeg-master-latest-win64-gpl.zip`, the `bin` folder)

   With Scoop: `scoop install vhs` (pulls in ttyd and ffmpeg).
   On macOS: `brew install vhs`.
2. Make sure `cargo`, `npx` and `python` work in a new terminal.
3. From the repo root: `powershell -File docs/demo/record.ps1`

`install --dry-run` runs against a copy of the sample config in
`docs/demo/home/`, so your own configs are never touched. The script copies
this repo's release build and that config to `C:\Users\Public\meiose-demo`
and runs from there, so no path in the GIF contains your user name; the folder
is removed afterwards.
