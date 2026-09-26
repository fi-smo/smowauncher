//! Runs a program and captures its output — without admin rights. Smowauncher usually runs
//! elevated, while extensions live in a folder any normal program can write to; running
//! them elevated would hand admin rights to whatever lands there. So when we're elevated,
//! the child gets a UAC-style filtered copy of our token (Administrators deny-only, no
//! extra privileges, medium integrity), exactly like apps started normally.

use std::os::windows::io::FromRawHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0};
use windows::Win32::Security::{
    CreateRestrictedToken, CreateWellKnownSid, DISABLE_MAX_PRIVILEGE, LUA_TOKEN, PSID, SECURITY_ATTRIBUTES,
    SID_AND_ATTRIBUTES, SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel, WinBuiltinAdministratorsSid, WinMediumLabelSid,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess,
    OpenProcessToken, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub code: Option<u32>,
}

/// Quotes one argument for the Windows command line (CommandLineToArgvW rules).
fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            c => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// Runs `program args…` in `cwd` with `env` added to our environment. The process is killed
/// after `timeout`.
pub fn run(program: &str, args: &[String], cwd: &std::path::Path, env: &[(String, String)], timeout: Duration) -> Result<Output, String> {
    let cmdline = std::iter::once(quote(program)).chain(args.iter().map(|a| quote(a))).collect::<Vec<_>>().join(" ");
    let mut block: Vec<u16> = Vec::new();
    // PSModulePath is left out: one set by PowerShell 7 makes Windows PowerShell 5.1 load
    // the wrong modules (and vice versa); each computes its own default when it's unset.
    let mut vars: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("PSModulePath") && !env.iter().any(|(e, _)| e.eq_ignore_ascii_case(k)))
        .collect();
    vars.extend(env.iter().cloned());
    for (k, v) in vars {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    block.push(0);

    unsafe {
        let token = unelevated_token()?;
        let sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, bInheritHandle: true.into(), ..Default::default() };
        let (mut out_r, mut out_w, mut err_r, mut err_w) = (HANDLE::default(), HANDLE::default(), HANDLE::default(), HANDLE::default());
        CreatePipe(&mut out_r, &mut out_w, Some(&sa), 0).map_err(|e| e.to_string())?;
        CreatePipe(&mut err_r, &mut err_w, Some(&sa), 0).map_err(|e| e.to_string())?;
        // Only the child's ends are inherited.
        let _ = SetHandleInformation(out_r, HANDLE_FLAG_INHERIT.0, windows::Win32::Foundation::HANDLE_FLAGS(0));
        let _ = SetHandleInformation(err_r, HANDLE_FLAG_INHERIT.0, windows::Win32::Foundation::HANDLE_FLAGS(0));

        let si = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            dwFlags: STARTF_USESTDHANDLES,
            hStdOutput: out_w,
            hStdError: err_w,
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        let mut cmd: Vec<u16> = cmdline.encode_utf16().chain(std::iter::once(0)).collect();
        let cwd_w: Vec<u16> = cwd.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        let created = CreateProcessAsUserW(
            Some(token),
            PCWSTR::null(),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            true,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            Some(block.as_ptr() as *const _),
            PCWSTR(cwd_w.as_ptr()),
            &si,
            &mut pi,
        );
        let _ = CloseHandle(token);
        let _ = CloseHandle(out_w);
        let _ = CloseHandle(err_w);
        if let Err(e) = created {
            let _ = CloseHandle(out_r);
            let _ = CloseHandle(err_r);
            return Err(format!("couldn't start {program}: {e}"));
        }
        let _ = CloseHandle(pi.hThread);

        // Read both pipes on threads so a chatty stderr can't block stdout.
        let reader = |h: HANDLE| {
            let h = h.0 as isize;
            std::thread::spawn(move || {
                use std::io::Read;
                let mut f = std::fs::File::from_raw_handle(h as *mut _);
                let mut buf = Vec::new();
                let _ = f.by_ref().take(4 * 1024 * 1024).read_to_end(&mut buf);
                buf
            })
        };
        let (out_t, err_t) = (reader(out_r), reader(err_r));

        let start = Instant::now();
        let finished = WaitForSingleObject(pi.hProcess, timeout.as_millis() as u32) == WAIT_OBJECT_0;
        if !finished {
            let _ = TerminateProcess(pi.hProcess, 1);
            let _ = WaitForSingleObject(pi.hProcess, 2000);
        }
        let mut code = 0u32;
        let code = (finished && GetExitCodeProcess(pi.hProcess, &mut code).is_ok()).then_some(code);
        let _ = CloseHandle(pi.hProcess);
        let stdout = out_t.join().unwrap_or_default();
        let stderr = err_t.join().unwrap_or_default();
        if !finished {
            return Err(format!("timed out after {:.0} s", start.elapsed().as_secs_f32()));
        }
        Ok(Output { stdout, stderr, code })
    }
}

/// Our token without admin rights (a copy of it when we aren't elevated anyway).
unsafe fn unelevated_token() -> Result<HANDLE, String> {
    unsafe {
        let mut own = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
            &mut own,
        )
        .map_err(|e| e.to_string())?;
        let mut admins = [0u8; 68];
        let mut size = admins.len() as u32;
        let admins_sid = PSID(admins.as_mut_ptr() as *mut _);
        CreateWellKnownSid(WinBuiltinAdministratorsSid, None, Some(admins_sid), &mut size).map_err(|e| e.to_string())?;
        let disable = [SID_AND_ATTRIBUTES { Sid: admins_sid, Attributes: 0 }];
        let mut restricted = HANDLE::default();
        let r = CreateRestrictedToken(own, DISABLE_MAX_PRIVILEGE | LUA_TOKEN, Some(&disable), None, None, &mut restricted);
        let _ = CloseHandle(own);
        r.map_err(|e| e.to_string())?;
        // Medium integrity, like a normally started app.
        let mut medium = [0u8; 68];
        let mut size = medium.len() as u32;
        let medium_sid = PSID(medium.as_mut_ptr() as *mut _);
        CreateWellKnownSid(WinMediumLabelSid, None, Some(medium_sid), &mut size).map_err(|e| e.to_string())?;
        let label = TOKEN_MANDATORY_LABEL { Label: SID_AND_ATTRIBUTES { Sid: medium_sid, Attributes: 0x20 /* SE_GROUP_INTEGRITY */ } };
        SetTokenInformation(restricted, TokenIntegrityLevel, &label as *const _ as *const _, size_of::<TOKEN_MANDATORY_LABEL>() as u32 + size)
            .map_err(|e| e.to_string())?;
        Ok(restricted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote("two words"), "\"two words\"");
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(r"C:\path with\"), r#""C:\path with\\""#);
        assert_eq!(quote(""), "\"\"");
    }

    #[test]
    fn runs_and_captures() {
        let out = run(
            "cmd.exe",
            &["/c".into(), "echo %SMOW_TEST%& echo err 1>&2".into()],
            &std::env::temp_dir(),
            &[("SMOW_TEST".into(), "hello".into())],
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
        assert!(String::from_utf8_lossy(&out.stderr).contains("err"));
        assert_eq!(out.code, Some(0));
    }
}
