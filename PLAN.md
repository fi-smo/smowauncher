# Smowauncher — Plan

A fast, low-memory, keyboard-first launcher for Windows 11 (Raycast / Flow Launcher style).
Tapping the Win key opens it instead of the Start menu.

## Decisions (agreed)

| Topic | Decision |
|---|---|
| Stack | Rust + Slint UI + `windows` crate (Win32 APIs) |
| Win key | Tapping Win alone opens Smowauncher. Combos (Win+E, Win+Shift+S…) pass through. |
| Look | Raycast-like: dark, compact, rounded, subtle translucent blur, footer with action hints |
| Files | Everything integration, results mixed with apps (apps first). Prefix `f ` / `/` = files only |
| Calc / units / currency | Locale from Windows region settings. Rates from Frankfurter (ECB), free, no key, cached daily |
| v1 extras | System commands, web search fallback, window switcher, clipboard history |
| Settings | `%APPDATA%\Smowauncher\config.toml` with hot reload. Settings UI comes later |
| Privileges | Autostart via an elevated Task Scheduler task. Launched apps are **de-elevated** |
| Distribution | Portable single `.exe` that registers its own autostart |

## Performance budgets (these are requirements)

| Metric | Target |
|---|---|
| Win tap → window visible & focused | < 30 ms (hard max 50 ms) |
| Keystroke → app results | < 5 ms |
| Keystroke → Everything results | < 50 ms |
| Idle RAM (window hidden) | < 30 MB private working set, goal ~15 MB |
| Idle CPU | 0 % — no polling timers, everything is event-driven |
| Cold start → ready | < 300 ms |
| Binary size | < 15 MB |

How we hit them:
- **The window is created once and hidden/shown, never destroyed.** Showing it is just `ShowWindow` plus a repaint.
- **Slint renderer is chosen by measurement in M0.** The software renderer avoids a GPU context (~20–40 MB); a ~750×480 window renders easily on the CPU. femtovg/Skia are used only if the numbers justify them.
- **Trim the working set on hide** (`SetProcessWorkingSetSize(-1,-1)`) and load icons lazily from a disk cache.
- **No async runtime (no tokio).** A few dedicated threads talk over `crossbeam-channel`, and HTTP goes through blocking `ureq` on a worker thread.
- **No SQLite.** Usage stats and clipboard history live in small serde files.
- Release profile: `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `opt-level = 3`, `strip = true`.
- **Benchmarks in CI** (criterion) for fuzzy match over 3k items and for calc parsing.

## Architecture

```
┌─────────────────────────── smowauncher.exe (single process) ───────────────────────────┐
│                                                                                        │
│  Hook thread ──(WinTap)──▶  UI thread (Slint event loop)  ◀──results── Query engine    │
│  WH_KEYBOARD_LL              window, tray, show/hide           │  fans out to providers│
│                                                                ▼                       │
│  Clipboard listener      Providers (trait Provider):                                   │
│  (hidden msg window)     Apps · Files(Everything) · Calc/Units/Currency · System ·     │
│                          WebSearch · Windows · Clipboard                               │
│                                                                                        │
│  Background workers: app indexer + icon cache, rate fetcher, config watcher            │
└────────────────────────────────────────────────────────────────────────────────────────┘
```

### Provider trait (the future plugin boundary)
```rust
trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    fn prefixes(&self) -> &[&str];                    // e.g. ["f ", "/"] for files
    fn query(&self, q: &Query, cx: &QueryCx) -> Vec<Item>;  // fast, synchronous
    fn query_async(&self, q: &Query, cx: &QueryCx, sink: Sink) {} // slow sources (Everything)
    fn actions(&self, item: &Item) -> Vec<Action>;    // Ctrl+K action panel
}
```
Every query gets a generation ID, so results from stale queries are discarded. Fast providers run inline on each keystroke. Slow ones run on a worker, are debounced (~25 ms) and cancelled when the next query arrives. Results merge by `score = match_score + frecency_boost + provider_weight`.

Later, external plugins can implement the same contract out-of-process (JSON-RPC over stdio, like Flow Launcher). A crashing plugin then can't take down the launcher, and it adds no RAM while unused.

## Feature design

### 1. Win-key replacement (highest risk, done first)
- A `WH_KEYBOARD_LL` hook runs on its own thread with its own message loop.
- On LWin/RWin down: set `win_pending = true` and let the event pass.
- Any other key while Win is held clears the flag (it's a combo, so Windows handles it).
- On Win up with `win_pending` still set: **inject a dummy key** (`VK 0xE8`, unassigned) *before* the Win-up passes. Start sees a "combo" and stays closed. Then post `ShowLauncher` to the UI thread. This is the proven AutoHotkey technique.
- A mouse click while Win is held also clears the flag (a mouse hook, or check `GetAsyncKeyState`).
- The hook callback must return quickly. If it goes over `LowLevelHooksTimeout`, Windows silently removes the hook. It only sets flags and posts messages.
- Because our process produced the last input (the dummy key), `SetForegroundWindow` is allowed. That makes focus reliable without hacks.
- Failure is safe: if the process dies, the hook disappears and Win opens Start again.
- Escape hatches: a tray menu item "Disable Win key capture", `win_key = false` in config, and a secondary hotkey (default `Alt+Space`) via `RegisterHotKey`.

### 2. Privileges
- `smowauncher.exe --install` creates a scheduled task: at logon, "Run with highest privileges", no time limit. `--uninstall` removes it.
- Running elevated lets the hook see keys even when an admin window has focus.
- **Launching apps de-elevated:** use the desktop shell's `IShellDispatch2::ShellExecute`, obtained through `IShellWindows` → `FindWindowSW` → `IServiceProvider`. Explorer then launches the app with the normal user token. "Run as administrator" is an explicit action (`runas` verb).

### 3. Apps
- Enumerate `shell:AppsFolder` (`FOLDERID_AppsFolder`, `IShellItem`). This returns exactly what Start shows: Win32 shortcuts **and** Store/UWP apps (AUMIDs).
- Also index `.lnk` files in both Start Menu folders for target paths and arguments, plus a user-configurable list of extra folders.
- Icons come from `IShellItemImageFactory::GetImage` at 32 px (and 64 px for high DPI), cached as raw RGBA under `%LOCALAPPDATA%\Smowauncher\icons`. They load only when a row becomes visible.
- Updates: `ReadDirectoryChangesW` on the Start Menu folders plus a cheap re-enumeration of AppsFolder when the window opens (throttled to at most once every 60 s).
- Matching: `nucleo-matcher` (fuzzy with smart case). Acronyms are boosted ("vsc" → Visual Studio Code), plus word-start and prefix bonuses.
- Ranking learns from use: frecency (launch count with time decay) plus query→item bindings (typing "ch" and choosing Chrome makes "ch" favor Chrome).

### 4. Files & folders (Everything)
- Talk to Everything directly over its **IPC protocol** (`WM_COPYDATA`, `EVERYTHING_IPC_QUERY2`). This is about 300 lines of Rust and needs no bundled DLL, which keeps the single-exe goal.
- Support the Everything 1.4 and 1.5 window classes / instance names (the 1.5 alpha uses the `"1.5a"` instance).
- Query with a limit of ~20, requesting full path + date modified, sorted by Everything's run count / recency.
- With mixed results, only apps show at 1–2 characters. File results join at 3+ characters, under the apps.
- Actions: Open, Open containing folder (selects the file in Explorer), Copy path, Copy file, Properties, Open with…
- If Everything isn't running, show a single hint row: "Start Everything to search files".

### 5. Calculator, units, currency
- Use **`fend-core`** (pure Rust, no dependencies). It handles arithmetic, functions, `%`, hex/bin, and a full unit system: `10 m to ft`, `2 l in oz`, `100 km/h to mph`, `30 C to F`.
- A cheap prefilter only calls fend when the query looks like math or a conversion, so app searches never pay for it.
- Locale: read `LOCALE_SDECIMAL` / `LOCALE_STHOUSAND`. If the decimal separator is `,`, convert `3,5` → `3.5` before evaluating and format output the same way.
- **Currency:** fend's exchange-rate hook reads rates from our cache. A worker fetches `api.frankfurter.app/latest` once a day (and on startup if stale), storing JSON in `%APPDATA%`, so conversion works offline. Aliases: `$`→USD, `€`→EUR, `zł`→PLN, `£`→GBP, `euro`, `dollars`…
- A bare amount like `100 usd` converts to the **default currency from Windows region settings**, plus 2–3 favorites from config.
- Enter copies the result. Alt+Enter copies the expression and the result.

### 6. System commands
- Lock, Sleep, Hibernate, Sign out, Shut down, Restart (shutdown/restart ask for confirmation), Empty Recycle Bin, Open Task Manager.
- Settings pages through `ms-settings:` URIs with keywords (`bluetooth`, `display`, `sound`, `wifi`, `apps`, `update`…).

### 7. Web search fallback
- When nothing strong matches, the last row reads "Search Google for '…'".
- Configurable prefixes: `g `, `ddg `, `yt `, `gh `, `w ` (Wikipedia) …, each with a URL template.

### 8. Window switcher
- `EnumWindows`, keeping windows that are visible, have a title, aren't tool windows, and aren't cloaked (`DWMWA_CLOAKED`, which hides other virtual desktops and suspended UWP apps).
- Mixed into the results with a "Switch to" badge. The prefix `w:` or `<` shows windows only.
- Activation restores the window if it's minimized, then calls `SetForegroundWindow`.

### 9. Clipboard history
- Uses `AddClipboardFormatListener` on a hidden message window (event-driven, zero CPU when idle).
- Stores text only in v1: the last 200 entries, de-duplicated, persisted to disk.
- **Privacy:** skip content marked `ExcludeClipboardContentFromMonitorProcessing` or `CanIncludeInClipboardHistory = 0` (password managers set these). There's also a configurable app blocklist.
- Opened with the `clip` keyword or its own hotkey (default `Ctrl+Alt+V`). Enter pastes into the previously focused window: set the clipboard, restore focus, then `SendInput` Ctrl+V.

## UI / visual design (Raycast-like)

```
╭──────────────────────────────────────────────────────────╮
│  🔍  vsc                                                 │  56 px search bar, 18 px text
├──────────────────────────────────────────────────────────┤
│  APPLICATIONS                                            │  section headers, 11 px caps
│ ▌[icon] Visual Studio Code                 Application   │  selected row: soft highlight
│  [icon] Visual Studio Installer            Application   │
│  FILES                                                   │
│  [icon] vscode-settings.json   E:\dotfiles    Modified 2d│
│  CALCULATOR                                              │
│  [ =  ] 12 m → 39.37 ft                          Copy    │
├──────────────────────────────────────────────────────────┤
│  [logo] Smowauncher          Open ↵    Actions  Ctrl K   │  footer: action hints
╰──────────────────────────────────────────────────────────╯
```
- ~760 px wide. Height grows with the result count (max ~8 rows visible). Positioned in the upper third of the **monitor that contains the cursor**.
- Frameless. Win11 rounded corners (`DWMWA_WINDOW_CORNER_PREFERENCE`). Acrylic backdrop (`DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_TRANSIENTWINDOW`) with a dark tinted overlay. If transparency doesn't work well with Slint, fall back to a solid `#1C1C1E` with a 1 px border — still Raycast-looking.
- Font: Segoe UI Variable. Colors are tokens in one Slint `Theme` global, so a light theme and custom themes come cheaply later.
- Motion: a 90–120 ms fade + 4 px slide on show. Instant hide (hiding should never feel slow).
- Keyboard: ↑/↓ or Ctrl+J/K, Enter, Ctrl+Enter (open folder), Ctrl+K (action panel), Esc (clear the query, then hide), Tab (autocomplete). The window hides on focus loss.
- A calculator/conversion result also appears as a large "hero" card at the top when the query is math.

## Project layout

```
smowauncher/
├─ Cargo.toml
├─ build.rs                    # slint-build, exe manifest + icon (embed-resource)
├─ ui/                         # .slint files: app-window, result-row, action-panel, theme
├─ src/
│  ├─ main.rs                  # single-instance mutex, CLI (--install/--uninstall), boot
│  ├─ app.rs                   # UI glue, show/hide, query dispatch, generation IDs
│  ├─ platform/                # hook.rs, window_fx.rs (DWM), shell_exec.rs (de-elevate),
│  │                           # tray.rs, autostart.rs, locale.rs, clipboard.rs
│  ├─ engine/                  # query.rs, ranking.rs, frecency.rs, provider.rs
│  ├─ providers/               # apps/, files_everything/, calc.rs, currency.rs,
│  │                           # system.rs, websearch.rs, windows.rs, clipboard.rs
│  ├─ config.rs                # TOML schema + defaults + hot reload
│  └─ icons.rs                 # extraction + disk cache
└─ benches/
```
Main crates: `slint`, `windows`, `fend-core`, `nucleo-matcher`, `serde`, `toml`, `ureq`, `crossbeam-channel`, `tracing` (+ file appender). The tray icon is built directly on `Shell_NotifyIcon` to keep it lightweight.

## Milestones

**M0 — Spike: de-risk the hard parts (1–2 sessions)**
1. A Slint frameless window with rounded corners + acrylic, shown/hidden instantly. Measure idle RAM with each renderer and pick one.
2. The Win-tap hook with the dummy-key trick: verify Start stays closed, combos still work, focus is reliable.
3. An elevated run + de-elevated `ShellExecute` via the explorer shell.
✅ Exit criteria: Win tap shows the window in < 30 ms, idle stays under 30 MB, Win+E still works.

**M1 — Core launcher**
App index (AppsFolder + .lnk), icon cache, fuzzy search, launching, frecency, tray icon, config.toml + hot reload, single instance, `--install` autostart task.

**M2 — Files**
Everything IPC client, mixed results, action panel (Ctrl+K), file actions.

**M3 — Calculator / units / currency**
fend-core integration, locale handling, Frankfurter fetch + cache, currency aliases, hero result card.

**M4 — System commands, web search, window switcher**

**M5 — Clipboard history**

**M6 — Polish & performance pass**
Animations, a light theme, a full measurement pass against the budget table, edge cases (multi-monitor, DPI changes, fullscreen games → don't steal Win key while a game is fullscreen, configurable), logging, crash safety.

**Later:** settings UI, out-of-process plugin API, AI commands, snippets, installer/MSIX + auto-update, file previews.

## Status

**M0 + M1 implemented (2026-09-24).** Measured on this machine (2× 2560×1440 @150%, NVIDIA GPU, 288 apps):

| Metric | Budget | Measured |
|---|---|---|
| Win tap → visible & focused | < 30 ms | 18 ms cold, 3 ms warm |
| Cold start → window ready | < 300 ms | 29 ms (index arrives at ~330 ms) |
| Idle RAM (hidden, trimmed) | < 30 MB | **8 MB private**, 0.1 MB working set |
| Binary size | < 15 MB | 9.5 MB |
| App enumeration | — | 313 ms (in the throwaway `--index` process) |

M0 findings:
- **Renderer:** femtovg/OpenGL costs ~150 MB private (NVIDIA GL driver) and a 750 ms startup. The Slint **software renderer** is the default: 8 MB, and acrylic still works because its premultiplied alpha reaches DWM through the extended frame.
- **Win-key tap** with the dummy-key trick: Start stays closed, and Win+R and other combos pass through.
- **Visibility via DWM cloaking** instead of SW_HIDE: no stale frame on show.
- **Frame buttons:** the frame must drop `WS_SYSMENU`, otherwise DWM draws a close button into the acrylic area.

Elevated autostart via the scheduled task is verified (installed by the user).

**M2 implemented (2026-09-24): files via Everything, action panel.**
- Everything IPC client (`src/files/everything.rs`), 1.4 and 1.5a window classes. Each search runs two queries (run count ↓, date modified ↓); the first batch is shown immediately (~30 ms), the second refines it (~25 ms later). Local ranking: name match > run count > recency, penalties for depth, AppData, dot-folders, build output and artifacts.
- **Pitfall:** Everything replies to a query *inside* our `SendMessage`. Sending the next query from the reply handler deadlocks, so the next stage is posted instead.
- Measured on this machine: ~25–30 ms per Everything query for specific terms (exclusions make no difference), ~250 ms for 1-character queries. This is why the default `min_chars = 3`.
- File icons: `SHGetFileInfo` by extension (no disk access), per file only for exe/lnk/ico/url. Extracted on a background thread.
- Ctrl+K action panel plus direct shortcuts. Properties / Open with run inside Explorer (`Folder.ParseName().InvokeVerb`).
- `--install` now copies the exe to `%LOCALAPPDATA%\Programs\Smowauncher`, and `--update` refreshes that copy without UAC.
- Dev aids: `--files-debug [--no-exclude] <query>`, `--icon-debug <path>`.

**M3–M5 implemented (2026-09-24).**
- **M3 calculator** (`src/calc`): fend-core with a pre-filter. Only operators, functions, "to/in/as" conversions or bare currency amounts are evaluated, so "7zip", "3d builder" and "2048" stay app searches. Everyday spellings are rewritten: "72 f to c" → °F/°C, "2 l to oz" → fluid ounces. Frankfurter/ECB rates are cached in `%APPDATA%\Smowauncher\rates.json` and refreshed every 12 h via WinHTTP (no TLS crate). Results appear in a card with currency symbol chips and currency names. Locale: decimal separator and default currency come from Windows, with a `[calc] default_currency` override.
- **M4:** system commands (lock, sleep, hibernate, shut down/restart/sign out with a second-Enter confirmation, empty recycle bin) and 26 `ms-settings:` pages are injected into the app index. Web keyword searches (`[web]` config) and a fallback row; URL detection. Window switcher (`<` prefix, and up to 3 matches in mixed results; switch/close).
- **M5 clipboard history** (`src/clip.rs`): `AddClipboardFormatListener`, text only, skips content marked `ExcludeClipboardContentFromMonitorProcessing` / `CanIncludeInClipboardHistory=0` and apps in `ignore_apps`. Stored in `%LOCALAPPDATA%` (not roaming). Opened with `clip …` or Ctrl+Alt+V; Enter pastes into the previous window.
- **Pitfall:** processes started from a packaged (MSIX) app, such as the Claude desktop app's terminals, get their `%AppData%` writes silently redirected into `Packages\…\LocalCache`. `--update` therefore re-launches itself through Explorer when `GetCurrentPackageFullName` says it runs inside a package.
- **No-Everything fallback:** Windows Search index over OLE DB (`src/files/wsearch.rs`) in a `--wsearch` helper process that lives only while the launcher is in use (trimmed on hide, exits after 60 s idle). Measured: ~55 ms to connect, then ~13 ms per query. Covers indexed folders only, so a hint row under the results offers to start Everything, or to install it with `winget install voidtools.Everything` (falls back to the website without winget), then polls until Everything runs. `[files] windows_search = false` disables it. `SMOW_NO_EVERYTHING=1` simulates a machine without Everything.
- Dev aids: `--calc-debug <expr>…`, `--wsearch-debug <query>`, `--preview "<query>" out.bmp` (renders the window without hooks or focus and saves a screenshot), `--via-explorer <cmd> <args>`.

Dev helpers: `tools/devtools.ps1` (inject keys, screenshots, memory). `smowauncher.exe --quit` stops a running instance.

**M6 measurement pass (2026-09-24, all features on, 288 apps + 33 commands):**

| Metric | Budget | Measured |
|---|---|---|
| Win tap → visible & focused | < 30 ms | 3–7 ms warm, 17–20 ms first show (real-usage log) |
| Keystroke → results (sync part) | < 5 ms | 0.02–0.63 ms ("c" worst case: largest fuzzy result set) |
| Keystroke → Everything files | < 50 ms | ~30 ms first batch, ~55 ms refined |
| Keystroke → Windows Search files | — | ~55 ms first query (connect), ~13 ms after |
| Idle RAM (hidden, trimmed) | < 30 MB | 7 MB private, 0.1 MB working set |
| Cold start → window ready | < 300 ms | 29 ms (app index at ~310 ms) |
| Binary size | < 15 MB | 11.1 MB |

**M6 so far:** light/dark theme (`[appearance] theme = "system" | "dark" | "light"`, follows Windows live via `WM_SETTINGCHANGE "ImmersiveColorSet"`); animated selection highlights; borderless-fullscreen game detection for the Win-key passthrough; keyboard hook re-installed every 10 min and on unlock/resume, which also resets Win state stuck by Win+L; panics logged with a backtrace; the logon task restarts the launcher on failure (applies after the next `--install`). The Win key is held back instead of cancelled; the hook trace in the log (`keys before …`) is used to diagnose the remaining second-tap report.

## Known risks
- **Slint + acrylic transparency on Windows**: verified in M0, and a solid-color fallback is designed in.
- **Win-key edge cases** (long hold, Win+mouse, Windows updates changing Start behavior): covered by M0 testing + escape hatches.
- **Fullscreen games**: tapping Win shouldn't pop up the launcher. Detect exclusive fullscreen with `SHQueryUserNotificationState` and pass through.
- **Everything 1.4 vs 1.5 IPC differences**: both instance names are supported.
- **Elevated process + UIPI**: drag-and-drop from normal apps into the launcher is blocked. That's not needed in v1.
