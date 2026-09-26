# Writing Smowauncher extensions

An extension adds its own results to the launcher. Type its **keyword** (and optionally
some text) and Smowauncher runs the extension's program, then shows the items it prints.

Extensions live in `%APPDATA%\Smowauncher\extensions\`, one folder each. Settings →
Extensions has buttons to open that folder, install the bundled examples (qBittorrent,
Jenkins, 2FA codes) and store secrets.

## Security

- Extensions run with **normal user rights** — never with Smowauncher's admin rights —
  because the extensions folder can be written by any program you run.
- Secrets (passwords, API tokens) are kept in **Windows Credential Manager**, not in the
  extension's files, and handed to the extension in environment variables when it runs.
- Only install extensions you trust: they run as you.

## `extension.toml`

```toml
name = "qBittorrent"                 # shown in results and settings
keyword = "qb"                       # type "qb" or "qb ubuntu"
description = "Torrents with progress"
command = "powershell.exe"           # a program on PATH, or a file in this folder
args = ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", "main.ps1"]
refresh = 2                          # re-run every 2 s while the list is shown (0 = off)
timeout = 10                         # seconds before the program is stopped
secrets = ["password"]               # asked for in Settings → Extensions (before [settings]!)

[settings]                           # free-form values for your script
url = "http://localhost:8080"
```

Note: `secrets` has to come **before** `[settings]` — everything after a `[table]` header
belongs to that table in TOML.

The program runs in the extension's folder with these environment variables:

| Variable | Value |
|---|---|
| `SMOW_QUERY` | The text typed after the keyword (may be empty) |
| `SMOW_ACTION` | Empty when searching; the `value` of a `run` action when one was chosen |
| `SMOW_SETTING_<NAME>` | Each `[settings]` entry (name upper-cased) |
| `SMOW_SECRET_<NAME>` | Each secret from Credential Manager (empty if not set) |
| `SMOW_EXTENSION_DIR` | The extension's folder |
| `SMOW_DATA_DIR` | A folder for the extension's own data (`%LOCALAPPDATA%\Smowauncher\extensions\<id>`) |

## Output

Print one JSON object to standard output (UTF-8):

```json
{
  "items": [
    {
      "title": "ubuntu-24.04.iso",
      "subtitle": "45% · ↓ 3.2 MB/s · ETA 4 min",
      "badge": "downloading",
      "icon": "",
      "progress": 0.45,
      "actions": [
        { "title": "Open Web UI", "type": "open", "value": "http://localhost:8080" },
        { "title": "Pause", "type": "run", "value": "pause:8c2f…" },
        { "title": "Copy magnet link", "type": "copy", "value": "magnet:?xt=…" }
      ]
    }
  ],
  "message": "Resumed",
  "query": null
}
```

- **items**: the results. Only `title` is required.
  - `icon`: a [Segoe Fluent Icons](https://learn.microsoft.com/windows/apps/design/style/segoe-fluent-icons-font)
    code point (e.g. `""`), or a short piece of text.
  - `progress`: 0–1 draws a progress bar under the row.
  - `actions`: Enter runs the first one; Ctrl+K lists all of them.
- **Action types**:
  - `copy` — copies `value` and closes the launcher.
  - `paste` — pastes `value` into the window that was active before.
  - `open` — opens `value` (a URL, file or folder).
  - `run` — runs the extension again with `SMOW_ACTION` = `value` and the same query; use it
    for things like "start build" or "pause". Print the updated list, and optionally a
    `message` for the status bar.
- **message**: shown in the status bar.
- **query**: replaces the text in the search box (`""` resets it to just the keyword).

If the program prints nothing, Smowauncher shows the first lines of its error output.

## Tips

- Windows PowerShell 5.1 reads scripts without a byte-order mark as ANSI. Save `.ps1` files
  as **UTF-8 with BOM** if they contain non-ASCII text, and set
  `[Console]::OutputEncoding = [Text.Encoding]::UTF8` before printing.
- `ConvertTo-Json` turns one-element arrays into plain objects in some cases; Smowauncher
  accepts either for `actions`, but wrap lists in `@(...)` and use
  `ConvertTo-Json -InputObject $result -Depth 6 -Compress`.
- Any language works: a Python script (`command = "python"`), a compiled program, a batch
  file — anything that prints JSON.
- The bundled examples (in the repository's `extensions\` folder) are good starting points.
