# Renders docs/demo.gif with VHS (https://github.com/charmbracelet/vhs).
# Run docs/demo/record.ps1, which fills the demo database first.
Output docs/demo.gif

Set Shell "powershell"
Set FontSize 18
Set Width 1300
Set Height 800
Set Padding 24
Set Theme "Catppuccin Mocha"
Set TypingSpeed 35ms
Set WindowBar Colorful

Hide
Type "$env:MEIOSE_FAKE_HOME = (Resolve-Path docs/demo/home).Path; function prompt { '> ' }; Clear-Host"
Enter
Sleep 1s
Show

Type "meiose install --dry-run"
Sleep 400ms
Enter
Sleep 4s

Type "meiose log -n 8"
Sleep 400ms
Enter
Sleep 5s

Type "meiose stats"
Sleep 400ms
Enter
Sleep 6s
