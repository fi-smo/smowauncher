# Smowauncher

A fast, low-memory, keyboard-first launcher for Windows 11, in the spirit of Raycast and Flow Launcher. Tap the **Windows key** and it opens instead of the Start menu.

- **Apps**, including Store apps and Steam/Epic games, with fuzzy search that learns what you pick
- **Files & folders** via [Everything](https://www.voidtools.com/), falling back to the Windows Search index
- **Calculator, units and currency**: `(12+8)*2.5`, `10 m to ft`, `72 f to c`, `100 eur to pln` (ECB rates, cached offline)
- **System commands** (lock, sleep, shut down…) and Windows Settings pages
- **Web search** with keywords (`yt lofi`, `gh slint`) and URL detection
- **Window switcher** (`<`), **clipboard history** (`clip` or Ctrl+Alt+V), **emoji picker** (`:heart` or `emoji heart`)
- **Aliases and pins**: Ctrl+K on an app → "Add alias…" (type `ff` for Firefox) or "Pin to top"
- **Win key** opens it on a tap, or on a double tap if you'd rather keep Start on a single tap
- **Ctrl+K action panel**: open with, show in folder (respects your default file manager), properties, copy path, run as admin…

Built in Rust with [Slint](https://slint.dev/) (software renderer) and the Win32 API. Idle memory is about 7 MB, and the window shows about 5 ms after the key press.

## Install

Download `smowauncher.exe` from the [latest release](https://github.com/fi-smo/smowauncher/releases/latest) and run:

```
smowauncher.exe --install
```

or build it yourself with `cargo build --release`.

`--install` copies the exe to `%LOCALAPPDATA%\Programs\Smowauncher` and registers a logon task with highest privileges, so the Windows key works over admin windows too. Apps you launch still start unelevated, through Explorer. `--update` refreshes the installed copy without a UAC prompt. `--uninstall` removes the task.

Open **Settings** from the tray icon, by typing "settings" in the launcher, or with Ctrl+, while it's open. Everything is stored in `%APPDATA%\Smowauncher\config.toml` (edits there reload on save, and the settings window keeps your comments).

## Updates and releases

The installed copy checks GitHub Releases a minute after startup and every 12 hours, plus on demand from the tray ("Check for updates"). When it finds a newer version, it downloads it, verifies it against the published SHA-256, and swaps it in the next time the launcher is hidden. The new version starts with the same privileges. Turn this off with `[updates] enabled = false`.

To publish a release, bump `version` in `Cargo.toml`, commit, and push a matching tag:

```
git tag v0.3.0
git push origin v0.3.0
```

The Release workflow tests, builds, and attaches `smowauncher.exe` and `smowauncher.exe.sha256` to the release.

See [PLAN.md](PLAN.md) for the design, the performance budgets and the measurements.
