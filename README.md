# Smowauncher

A fast, low-memory, keyboard-first launcher for Windows 11, in the spirit of Raycast and Flow Launcher. Tap the **Windows key** and it opens instead of the Start menu.

- **Apps**, including Store apps and Steam/Epic games, with fuzzy search that learns what you pick
- **Files & folders** via [Everything](https://www.voidtools.com/), falling back to the Windows Search index
- **Calculator, units and currency**: `(12+8)*2.5`, `10 m to ft`, `72 f to c`, `100 eur to pln` (ECB rates, cached offline)
- **System commands** (lock, sleep, shut down…) and Windows Settings pages
- **Web search** with keywords (`yt lofi`, `gh slint`) and URL detection
- **Window switcher** (`<`), **clipboard history** (`clip` or Ctrl+Alt+V)
- **Ctrl+K action panel**: open with, show in folder (respects your default file manager), properties, copy path, run as admin…

Built in Rust with [Slint](https://slint.dev/) (software renderer) and the Win32 API. Idle memory is about 7 MB, and the window shows about 5 ms after the key press.

## Install

```
cargo build --release
target\release\smowauncher.exe --install
```

`--install` copies the exe to `%LOCALAPPDATA%\Programs\Smowauncher` and registers a logon task with highest privileges, so the Windows key works over admin windows too. Apps you launch still start unelevated, through Explorer. `--update` refreshes the installed copy without a UAC prompt. `--uninstall` removes the task.

Settings live in `%APPDATA%\Smowauncher\config.toml` and reload on save.

See [PLAN.md](PLAN.md) for the design, the performance budgets and the measurements.
