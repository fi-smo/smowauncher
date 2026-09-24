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

/// Does the actual work; must run elevated.
pub fn install_elevated() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
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

    // Replace a running (possibly non-elevated) instance with the task-started one.
    if instance::signal(input::quit_message()) {
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
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
