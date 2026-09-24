//! Autostart through Task Scheduler ("Run with highest privileges" at logon).
//! A task — unlike the Run registry key — can start elevated without a UAC prompt.

use super::{input, instance, shell};
use std::os::windows::process::CommandExt;
use std::process::Command;

const TASK_NAME: &str = "Smowauncher";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn task_xml(user: &str, exe: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Starts Smowauncher at logon with elevated rights so the Windows key works in every window.</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
    </Exec>
  </Actions>
</Task>
"#,
        user = xml_escape(user),
        exe = xml_escape(exe),
    )
}

fn schtasks(args: &[&str]) -> Result<(), String> {
    let out = Command::new("schtasks.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// `--install`: registers the task (asking for elevation if needed) and starts it.
pub fn install() -> Result<String, String> {
    if shell::is_elevated() {
        install_elevated()?;
    } else {
        match shell::run_self_elevated("--install-elevated")? {
            0 => {}
            code => return Err(format!("Elevated installer exited with code {code}.")),
        }
    }
    Ok("Smowauncher will now start automatically (with admin rights) when you sign in.".into())
}

/// Where the installed copy lives. Running from a copy (not the build output) means
/// rebuilding never fights the running launcher for the exe, and the task never points
/// into a build folder.
pub fn install_dir() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join(r"Programs\Smowauncher")
}

/// Asks a running instance to quit and waits (up to 3 s) for it to go away.
fn stop_running_instance() {
    if !instance::signal(input::quit_message()) {
        return;
    }
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !instance::is_running() {
            // The process may still be releasing its exe for a moment.
            std::thread::sleep(std::time::Duration::from_millis(150));
            return;
        }
    }
    log::warn!("running instance did not quit in time");
}

/// Copies this exe into the install folder (unless it already runs from there).
fn copy_self() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = install_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let target = dir.join("smowauncher.exe");
    let same = std::fs::canonicalize(&exe).ok() == std::fs::canonicalize(&target).ok();
    if !same {
        let mut last = String::new();
        for _ in 0..20 {
            match std::fs::copy(&exe, &target) {
                Ok(_) => return Ok(target),
                Err(e) => last = e.to_string(),
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        return Err(format!("copy to {}: {last}", target.display()));
    }
    Ok(target)
}

/// `--update`: replaces the installed copy with this exe and restarts it. Needs no UAC,
/// because the scheduled task already exists and only its target file changes.
pub fn update() -> Result<(), String> {
    if !install_dir().join("smowauncher.exe").exists() {
        return Err("Smowauncher isn't installed yet — run it with --install first.".into());
    }
    stop_running_instance();
    copy_self()?;
    schtasks(&["/Run", "/TN", TASK_NAME])
}

/// Does the actual work; must run elevated.
pub fn install_elevated() -> Result<(), String> {
    stop_running_instance();
    let exe = copy_self()?;
    let user = format!(
        "{}\\{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").unwrap_or_default()
    );
    let xml = task_xml(&user, &exe.to_string_lossy());
    // schtasks expects UTF-16 with BOM for encoding="UTF-16".
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(|u| u.to_le_bytes()));
    let path = std::env::temp_dir().join("smowauncher-task.xml");
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let result = schtasks(&["/Create", "/TN", TASK_NAME, "/XML", &path.to_string_lossy(), "/F"]);
    let _ = std::fs::remove_file(&path);
    result?;
    schtasks(&["/Run", "/TN", TASK_NAME])
}

/// `--uninstall`: removes the task.
pub fn uninstall() -> Result<String, String> {
    if shell::is_elevated() {
        uninstall_elevated()?;
    } else {
        match shell::run_self_elevated("--uninstall-elevated")? {
            0 => {}
            code => return Err(format!("Elevated uninstaller exited with code {code}.")),
        }
    }
    Ok("Smowauncher will no longer start automatically.".into())
}

pub fn uninstall_elevated() -> Result<(), String> {
    schtasks(&["/Delete", "/TN", TASK_NAME, "/F"])
}
