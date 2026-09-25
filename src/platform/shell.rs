//! Launching things. Because Smowauncher normally runs elevated (so the Win-key hook sees
//! input in admin windows), apps are started through Explorer's own `IShellDispatch2`,
//! which gives them Explorer's normal, non-elevated token.

use super::wide;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, IDispatch, IServiceProvider,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{
    CSIDL_DESKTOP, Folder2, IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellView,
    IShellWindows, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
    SHELLEXECUTEINFOW, SID_STopLevelBrowser, SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH,
    ShellExecuteExW, ShellWindows,
};
use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow, SW_SHOWNORMAL};
use windows::Win32::System::Variant::{
    VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BSTR, VT_I4, VariantClear,
};
use windows::core::{BSTR, Interface, PCWSTR};

/// Owned VARIANT that is cleared (freeing its BSTR) on drop.
struct Var(VARIANT);

impl Var {
    fn i4(v: i32) -> Self {
        Self::with(VT_I4, VARIANT_0_0_0 { lVal: v })
    }
    fn bstr(s: &str) -> Self {
        Self::with(VT_BSTR, VARIANT_0_0_0 { bstrVal: std::mem::ManuallyDrop::new(BSTR::from(s)) })
    }
    fn empty() -> Self {
        Self(VARIANT::default())
    }
    fn with(vt: windows::Win32::System::Variant::VARENUM, value: VARIANT_0_0_0) -> Self {
        Self(VARIANT {
            Anonymous: VARIANT_0 {
                Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                    vt,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: value,
                }),
            },
        })
    }
}

impl Drop for Var {
    fn drop(&mut self) {
        unsafe {
            let _ = VariantClear(&mut self.0);
        }
    }
}

pub fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Open,
    RunAs,
}

/// Launches in the background so the UI never waits on the shell.
pub fn launch(file: String, args: Option<String>, verb: Verb) {
    // We hold the foreground right now; let the launched app take it.
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    let _ = std::thread::Builder::new().name("launch".into()).stack_size(512 * 1024).spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        }
        let t = std::time::Instant::now();
        if verb == Verb::Open && is_elevated() {
            match explorer_shell_execute(&file, args.as_deref()) {
                Ok(()) => {
                    log::info!("launch (de-elevated) {file} in {:?}", t.elapsed());
                    return;
                }
                Err(e) => log::warn!("launch: explorer route failed ({e}); falling back"),
            }
        }
        match shell_execute(&file, args.as_deref(), verb) {
            Ok(()) => log::info!("launch {file} in {:?}", t.elapsed()),
            Err(e) => log::error!("launch {file} failed: {e}"),
        }
    });
}

/// Shows `path` in the user's file manager. With Explorer the item gets selected; with a
/// replacement registered as the folder handler (File Pilot, Directory Opus, …) its
/// containing folder opens in that app, through the command line it registered.
pub fn show_in_folder(path: &str) {
    let parent = parent_folder(path);
    match folder_handler() {
        Some(handler) if !handler.is_explorer() => {
            log::info!("show in folder via {}", handler.exe);
            launch(handler.exe.clone(), Some(handler.args_for(parent)), Verb::Open);
        }
        _ => launch("explorer.exe".into(), Some(format!("/select,\"{path}\"")), Verb::Open),
    }
}

fn parent_folder(path: &str) -> &str {
    let trimmed = path.trim_end_matches('\\');
    match trimmed.rfind('\\') {
        Some(i) if i <= 2 => &path[..i + 1],
        Some(i) => &trimmed[..i],
        None => path,
    }
}

/// The command registered to open folders (`Directory\shell\<default verb>\command`).
#[derive(Debug, PartialEq)]
struct FolderHandler {
    exe: String,
    /// Argument template, e.g. `"%1"`.
    args: String,
}

impl FolderHandler {
    fn is_explorer(&self) -> bool {
        self.exe.to_lowercase().ends_with("explorer.exe")
    }

    fn args_for(&self, dir: &str) -> String {
        // A trailing backslash would escape the closing quote ("D:\" → D:").
        let dir = if dir.ends_with('\\') { format!("{dir}.") } else { dir.to_owned() };
        let mut args = self.args.clone();
        let mut substituted = false;
        for token in ["%1", "%V", "%v", "%L", "%l"] {
            if args.contains(token) {
                args = args.replace(token, &dir);
                substituted = true;
            }
        }
        if !substituted {
            args = format!("{args} \"{dir}\"").trim().to_owned();
        }
        args
    }

    /// Splits a registered command line into executable and argument template.
    fn parse(command: &str) -> Option<Self> {
        let c = command.trim();
        let (exe, args) = if let Some(rest) = c.strip_prefix('"') {
            let end = rest.find('"')?;
            (&rest[..end], rest[end + 1..].trim())
        } else {
            let lower = c.to_lowercase();
            let end = lower.find(".exe").map(|i| i + 4).unwrap_or(c.find(' ').unwrap_or(c.len()));
            (&c[..end], c[end..].trim())
        };
        (!exe.is_empty()).then(|| Self { exe: expand_env(exe), args: args.to_owned() })
    }
}

fn expand_env(s: &str) -> String {
    use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
    let src = wide(s);
    let mut buf = vec![0u16; 1024];
    let n = unsafe { ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), Some(&mut buf)) } as usize;
    if n == 0 || n > buf.len() { s.to_owned() } else { String::from_utf16_lossy(&buf[..n - 1]) }
}

fn reg_string(root: windows::Win32::System::Registry::HKEY, subkey: &str, value: Option<&str>) -> Option<String> {
    use windows::Win32::System::Registry::{RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW};
    let key = wide(subkey);
    let value = value.map(wide);
    let mut buf = vec![0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            root,
            PCWSTR(key.as_ptr()),
            value.as_ref().map(|v| PCWSTR(v.as_ptr())).unwrap_or(PCWSTR::null()),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut len),
        )
    };
    if r.is_err() {
        return None;
    }
    let n = (len as usize / 2).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..n])).filter(|s| !s.is_empty())
}

/// Per-user registration first (that's where file-manager replacements put it; elevated
/// processes don't reliably get the per-user part of HKEY_CLASSES_ROOT), then machine-wide.
fn folder_handler() -> Option<FolderHandler> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    for (root, base) in [(HKEY_CURRENT_USER, r"Software\Classes\Directory\shell"), (HKEY_LOCAL_MACHINE, r"SOFTWARE\Classes\Directory\shell")] {
        let verb = reg_string(root, base, None).filter(|v| !v.eq_ignore_ascii_case("none")).unwrap_or_else(|| "open".into());
        if let Some(cmd) = reg_string(root, &format!(r"{base}\{verb}\command"), None) {
            return FolderHandler::parse(&cmd);
        }
    }
    None
}

fn shell_execute(file: &str, args: Option<&str>, verb: Verb) -> windows::core::Result<()> {
    let file_w = wide(file);
    let args_w = args.map(wide);
    let verb_w = wide(match verb {
        Verb::Open => "open",
        Verb::RunAs => "runas",
    });
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb_w.as_ptr()),
        lpFile: PCWSTR(file_w.as_ptr()),
        lpParameters: args_w.as_ref().map(|a| PCWSTR(a.as_ptr())).unwrap_or(PCWSTR::null()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }
}

/// Opens a terminal in `dir` (Windows Terminal if installed, else PowerShell).
pub fn open_terminal(dir: &str) {
    // A trailing backslash would escape the closing quote ("D:\" → D:").
    let dir = if dir.ends_with('\\') { format!("{dir}.") } else { dir.to_owned() };
    let wt = std::env::var_os("LOCALAPPDATA")
        .map(|l| std::path::PathBuf::from(l).join(r"Microsoft\WindowsApps\wt.exe"))
        .is_some_and(|p| p.exists());
    if wt {
        launch("wt.exe".into(), Some(format!("-d \"{dir}\"")), Verb::Open);
    } else {
        launch(
            "powershell.exe".into(),
            Some(format!("-NoExit -Command Set-Location -LiteralPath '{}'", dir.replace('\'', "''"))),
            Verb::Open,
        );
    }
}

/// Runs a shell verb ("properties", "openas", ...) on a file inside Explorer's process, so
/// dialogs are hosted by Explorer: unelevated, and they outlive our short-lived thread.
pub fn invoke_verb(path: &str, verb: &'static str) {
    let path = path.to_owned();
    // Let Explorer bring its dialog to the front.
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    let _ = std::thread::Builder::new().name("verb".into()).stack_size(512 * 1024).spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        }
        let result = (|| -> windows::core::Result<()> {
            let shell = explorer_shell()?;
            let trimmed = path.trim_end_matches('\\');
            let item = match trimmed.rfind('\\') {
                // Drive roots have no parent folder: use the folder's own item.
                None => {
                    let folder: Folder2 = unsafe { shell.NameSpace(&Var::bstr(&path).0)? }.cast()?;
                    unsafe { folder.Self_()? }
                }
                Some(i) => {
                    let parent = if i <= 2 { &path[..i + 1] } else { &trimmed[..i] };
                    let folder = unsafe { shell.NameSpace(&Var::bstr(parent).0)? };
                    unsafe { folder.ParseName(&BSTR::from(&trimmed[i + 1..]))? }
                }
            };
            unsafe { item.InvokeVerb(&Var::bstr(verb).0) }
        })();
        if let Err(e) = result {
            log::error!("verb {verb} on {path} failed: {e}");
        }
    });
}

/// Explorer's `Shell.Application` object, reached through the desktop window. Everything
/// invoked through it runs inside Explorer with its (normal, unelevated) token.
fn explorer_shell() -> windows::core::Result<IShellDispatch2> {
    unsafe {
        let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)?;
        let loc = Var::i4(CSIDL_DESKTOP as i32);
        let empty = Var::empty();
        let mut hwnd = 0i32;
        let disp: IDispatch =
            windows.FindWindowSW(&loc.0, &empty.0, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)?;
        let sp: IServiceProvider = disp.cast()?;
        let browser: IShellBrowser = sp.QueryService(&SID_STopLevelBrowser)?;
        let view: IShellView = browser.QueryActiveShellView()?;
        let background: IDispatch = view.GetItemObject(SVGIO_BACKGROUND)?;
        let folder_view: IShellFolderViewDual = background.cast()?;
        folder_view.Application()?.cast()
    }
}

/// Asks the desktop's Explorer instance to run the command (the classic
/// "launch unelevated from an elevated process" technique).
fn explorer_shell_execute(file: &str, args: Option<&str>) -> windows::core::Result<()> {
    unsafe {
        let shell = explorer_shell()?;
        shell.ShellExecute(
            &BSTR::from(file),
            &Var::bstr(args.unwrap_or("")).0,
            &Var::bstr("").0,
            &Var::bstr("open").0,
            &Var::i4(SW_SHOWNORMAL.0).0,
        )
    }
}

/// Standard "Select folder" dialog. Returns the chosen folder's path.
pub fn pick_folder(owner: Option<windows::Win32::Foundation::HWND>) -> Option<String> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoTaskMemFree};
    use windows::Win32::UI::Shell::{FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        dialog.Show(owner).ok()?; // cancelled → None
        let item = dialog.GetResult().ok()?;
        let path = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const _));
        s
    }
}

/// True when this process runs with an MSIX package identity (file writes to AppData are
/// then virtualized into the package's private folder).
pub fn in_package() -> bool {
    use windows::Win32::Foundation::APPMODEL_ERROR_NO_PACKAGE;
    use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    let mut len = 0u32;
    unsafe { GetCurrentPackageFullName(&mut len, None) != APPMODEL_ERROR_NO_PACKAGE }
}

/// Starts `file args` through Explorer (unelevated, outside any package) and returns.
pub fn run_via_explorer(file: &str, args: &str) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
    explorer_shell_execute(file, Some(args)).map_err(|e| e.message())
}

/// Re-runs this exe elevated with `args` (UAC prompt) and waits. Returns its exit code.
pub fn run_self_elevated(args: &str) -> Result<u32, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe_w = wide(&exe.to_string_lossy());
    let args_w = wide(args);
    let verb_w = wide("runas");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: PCWSTR(verb_w.as_ptr()),
        lpFile: PCWSTR(exe_w.as_ptr()),
        lpParameters: PCWSTR(args_w.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info).map_err(|e| e.message())?;
        let mut code = 1u32;
        if !info.hProcess.is_invalid() {
            WaitForSingleObject(info.hProcess, INFINITE);
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
            let _ = CloseHandle(info.hProcess);
        }
        Ok(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_handlers() {
        let fp = FolderHandler::parse(r#""C:\Users\me\AppData\Local\Voidstar\FilePilot\FPilot.exe" "%1""#).unwrap();
        assert_eq!(fp.exe, r"C:\Users\me\AppData\Local\Voidstar\FilePilot\FPilot.exe");
        assert!(!fp.is_explorer());
        assert_eq!(fp.args_for(r"C:\Users\me\Documents"), r#""C:\Users\me\Documents""#);
        assert_eq!(fp.args_for(r"D:\"), r#""D:\.""#);

        let opus = FolderHandler::parse(r#"C:\Program Files\GPSoftware\Directory Opus\dopusrt.exe /open "%1""#).unwrap();
        assert_eq!(opus.exe, r"C:\Program Files\GPSoftware\Directory Opus\dopusrt.exe");
        assert_eq!(opus.args_for(r"E:\x"), r#"/open "E:\x""#);

        let ex = FolderHandler::parse(r"C:\WINDOWS\Explorer.exe").unwrap();
        assert!(ex.is_explorer());
        assert_eq!(ex.args_for(r"E:\x"), r#""E:\x""#);
    }

    /// Run with `cargo test -- --ignored` to see what this machine has registered.
    #[test]
    #[ignore]
    fn print_folder_handler() {
        println!("{:?}", folder_handler());
    }

    #[test]
    fn parents() {
        assert_eq!(parent_folder(r"C:\a\b.txt"), r"C:\a");
        assert_eq!(parent_folder(r"C:\b.txt"), r"C:\");
        assert_eq!(parent_folder(r"C:\a\"), r"C:\");
    }
}
