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

/// Opens Explorer with `path` selected.
pub fn show_in_folder(path: &str) {
    launch("explorer.exe".into(), Some(format!("/select,\"{path}\"")), Verb::Open);
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
