# Images for the README

| File | Content | How it was made |
|---|---|---|
| `icon.png` | The app icon, 256×256, transparent corners | Drawn with CoreGraphics from the shapes in `scripts/make-icon.sh` |
| `menu.png` | The open menu in the menu bar | Manual screenshot |
| `install-terminal.png` | Terminal asking for the password after the helper install command | `grip shot --window <id>` |
| `open-anyway.png` | The Security section of Privacy & Security, cropped | `grip shot --app "System Settings"`, then `sips -c` to crop off the sidebar |

To retake a window shot, run `grip windows` to find the window id, then
`grip shot --window <id> -o docs/images/<name>.png`.
