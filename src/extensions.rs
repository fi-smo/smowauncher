//! Extensions: folders in `%APPDATA%\Smowauncher\extensions\`, each with an `extension.toml`
//! and a program or script. Typing an extension's keyword runs it (without admin rights, see
//! `platform::process`) and shows the items it prints as JSON. See docs/extensions.md.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Manifest {
    pub name: String,
    /// "qb" → type "qb" or "qb <text>".
    pub keyword: String,
    pub description: String,
    /// Program to run (relative to the extension folder or on PATH), e.g. "powershell.exe".
    pub command: String,
    pub args: Vec<String>,
    /// Re-run every N seconds while its results are shown (0 = only when the query changes).
    pub refresh: f32,
    /// Seconds before the program is stopped.
    pub timeout: f32,
    /// Passed as SMOW_SETTING_<NAME> environment variables.
    pub settings: BTreeMap<String, String>,
    /// Names of secrets kept in Windows Credential Manager (Settings → Extensions), passed
    /// as SMOW_SECRET_<NAME>.
    pub secrets: Vec<String>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            name: String::new(),
            keyword: String::new(),
            description: String::new(),
            command: String::new(),
            args: Vec::new(),
            refresh: 0.0,
            timeout: 10.0,
            settings: BTreeMap::new(),
            secrets: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Extension {
    /// Folder name.
    pub id: String,
    pub dir: PathBuf,
    pub manifest: Manifest,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Action {
    pub title: String,
    /// "copy", "paste", "open" (URL or path) or "run" (runs the extension again with
    /// SMOW_ACTION = value, e.g. to start a build).
    #[serde(rename = "type")]
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Item {
    pub title: String,
    pub subtitle: String,
    pub badge: String,
    /// A Segoe Fluent Icons code point ("") or a short text.
    pub icon: String,
    /// 0..1 draws a progress bar under the row.
    pub progress: Option<f32>,
    /// The first action runs on Enter; all of them are in Ctrl+K.
    #[serde(deserialize_with = "one_or_many")]
    pub actions: Vec<Action>,
}

/// Accepts a single object where a list is expected: PowerShell's ConvertTo-Json turns
/// one-element arrays into plain objects in some cases.
fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Action>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        Many(Vec<Action>),
        One(Action),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::Many(v) => v,
        OneOrMany::One(a) => vec![a],
    })
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Output {
    pub items: Vec<Item>,
    /// Shown in the status bar (e.g. "Build #42 started").
    pub message: String,
    /// Replace the query with this (e.g. after an action).
    pub query: Option<String>,
}

pub fn dir() -> PathBuf {
    crate::paths::config_dir().join("extensions")
}

pub fn secret_target(ext: &str, name: &str) -> String {
    format!("Smowauncher/ext/{ext}/{name}")
}

/// All extensions with a valid manifest, sorted by name.
pub fn load_all() -> Vec<Extension> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir()) else { return out };
    for e in entries.flatten() {
        let path = e.path().join("extension.toml");
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        match toml::from_str::<Manifest>(&text) {
            Ok(m) if !m.keyword.trim().is_empty() && !m.command.trim().is_empty() => out.push(Extension {
                id: e.file_name().to_string_lossy().into_owned(),
                dir: e.path(),
                manifest: Manifest { keyword: m.keyword.trim().to_lowercase(), ..m },
            }),
            Ok(_) => log::warn!("extension {}: needs a keyword and a command", path.display()),
            Err(e) => log::warn!("extension {}: {e}", path.display()),
        }
    }
    out.sort_by(|a, b| a.manifest.name.to_lowercase().cmp(&b.manifest.name.to_lowercase()));
    out
}

/// Runs the extension for `query` (or for an action) and parses what it prints.
pub fn run(ext: &Extension, query: &str, action: Option<&str>) -> Result<Output, String> {
    let m = &ext.manifest;
    let mut env: Vec<(String, String)> = vec![
        ("SMOW_QUERY".into(), query.into()),
        ("SMOW_ACTION".into(), action.unwrap_or_default().into()),
        ("SMOW_EXTENSION_DIR".into(), ext.dir.to_string_lossy().into_owned()),
        ("SMOW_DATA_DIR".into(), data_dir(ext).to_string_lossy().into_owned()),
    ];
    for (k, v) in &m.settings {
        env.push((format!("SMOW_SETTING_{}", k.to_uppercase()), v.clone()));
    }
    for name in &m.secrets {
        let value = crate::platform::credentials::read(&secret_target(&ext.id, name)).unwrap_or_default();
        env.push((format!("SMOW_SECRET_{}", name.to_uppercase()), value));
    }
    let local = ext.dir.join(&m.command);
    let program = if local.exists() { local.to_string_lossy().into_owned() } else { m.command.clone() };
    let timeout = Duration::from_secs_f32(m.timeout.clamp(1.0, 120.0));
    let out = crate::platform::process::run(&program, &m.args, &ext.dir, &env, timeout)?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = stdout.trim().trim_start_matches('\u{feff}');
    if json.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(if err.is_empty() { format!("no output (exit code {:?})", out.code) } else { first_lines(&err) });
    }
    serde_json::from_str(json).map_err(|e| format!("invalid output: {e}"))
}

/// Where an extension may keep its own files (not in its folder, which updates replace).
pub fn data_dir(ext: &Extension) -> PathBuf {
    let d = crate::paths::data_dir().join("extensions").join(&ext.id);
    let _ = std::fs::create_dir_all(&d);
    d
}

fn first_lines(s: &str) -> String {
    s.lines().take(3).collect::<Vec<_>>().join(" ").chars().take(300).collect()
}

/// Splits "kw rest" into the extension with keyword `kw` and the rest.
pub fn match_query<'q>(exts: &[Extension], query: &'q str) -> Option<(usize, &'q str)> {
    let t = query.trim_start();
    let (kw, rest) = match t.find(' ') {
        Some(i) => (&t[..i], t[i + 1..].trim()),
        None => (t, ""),
    };
    let i = exts.iter().position(|e| e.manifest.keyword.eq_ignore_ascii_case(kw))?;
    // "qb" alone only counts once typed exactly; "qbittorrent" is a normal search.
    Some((i, rest))
}

/// Bundled example extensions (Settings → Extensions → Install examples).
pub const EXAMPLES: &[(&str, &[(&str, &str)])] = &[
    (
        "qbittorrent",
        &[
            ("extension.toml", include_str!("../extensions/qbittorrent/extension.toml")),
            ("main.ps1", include_str!("../extensions/qbittorrent/main.ps1")),
        ],
    ),
    (
        "jenkins",
        &[
            ("extension.toml", include_str!("../extensions/jenkins/extension.toml")),
            ("main.ps1", include_str!("../extensions/jenkins/main.ps1")),
        ],
    ),
    (
        "2fa",
        &[("extension.toml", include_str!("../extensions/2fa/extension.toml")), ("main.ps1", include_str!("../extensions/2fa/main.ps1"))],
    ),
];

/// Copies the examples into the extensions folder (existing files are kept). Returns how
/// many were installed.
pub fn install_examples() -> Result<usize, String> {
    let mut n = 0;
    for (id, files) in EXAMPLES {
        let d = dir().join(id);
        if d.join("extension.toml").exists() {
            continue;
        }
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        for (name, text) in *files {
            // Windows PowerShell 5 reads BOM-less scripts as ANSI; write UTF-8 with a BOM.
            let text = text.trim_start_matches('\u{feff}');
            let bytes = if name.ends_with(".ps1") { [b"\xEF\xBB\xBF".as_slice(), text.as_bytes()].concat() } else { text.as_bytes().to_vec() };
            std::fs::write(d.join(name), bytes).map_err(|e| e.to_string())?;
        }
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(kw: &str) -> Extension {
        Extension { id: kw.into(), dir: PathBuf::new(), manifest: Manifest { keyword: kw.into(), ..Manifest::default() } }
    }

    #[test]
    fn queries() {
        let exts = vec![ext("qb"), ext("jk")];
        assert_eq!(match_query(&exts, "qb"), Some((0, "")));
        assert_eq!(match_query(&exts, "QB  ubuntu iso "), Some((0, "ubuntu iso")));
        assert_eq!(match_query(&exts, "jk deploy"), Some((1, "deploy")));
        assert_eq!(match_query(&exts, "qbittorrent"), None);
    }

    #[test]
    fn output_parsing() {
        let o: Output = serde_json::from_str(
            r#"{"items":[{"title":"a","progress":0.5,"actions":[{"title":"Copy","type":"copy","value":"123"}]}],"message":"hi"}"#,
        )
        .unwrap();
        assert_eq!(o.items[0].progress, Some(0.5));
        assert_eq!(o.items[0].actions[0].kind, "copy");
        assert_eq!(o.message, "hi");
        let single: Output = serde_json::from_str(r#"{"items":[{"title":"a","actions":{"title":"Go","type":"open","value":"x"}}]}"#).unwrap();
        assert_eq!(single.items[0].actions.len(), 1);
    }

    #[test]
    fn examples_parse() {
        for (id, files) in EXAMPLES {
            let manifest: Manifest = toml::from_str(files[0].1).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(!manifest.keyword.is_empty() && !manifest.command.is_empty(), "{id}");
            // `secrets` must come before [settings] or TOML files it under settings.
            assert!(!manifest.settings.contains_key("secrets"), "{id}");
        }
    }
}
