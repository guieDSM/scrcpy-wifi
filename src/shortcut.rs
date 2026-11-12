//! "Install" the running exe into %LOCALAPPDATA%\scrcpy-wifi and add
//! Desktop and Start menu shortcuts to it.

use crate::install::data_dir;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const LINK_NAME: &str = "scrcpy Wi-Fi.lnk";

pub fn installed_exe() -> PathBuf {
    data_dir().join("scrcpy-wifi.exe")
}

fn start_menu_link() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(|a| PathBuf::from(a).join(r"Microsoft\Windows\Start Menu\Programs").join(LINK_NAME))
}

/// True when shortcuts are missing or point to a different build than the one running.
pub fn needs_setup() -> bool {
    let has_link = start_menu_link().is_some_and(|p| p.is_file());
    if !has_link || !installed_exe().is_file() {
        return true;
    }
    let Ok(current) = std::env::current_exe() else { return false };
    if same_file(&current, &installed_exe()) {
        return false;
    }
    match (fs::read(&current), fs::read(installed_exe())) {
        (Ok(a), Ok(b)) => a != b,
        _ => false,
    }
}

fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

pub fn create() -> Result<String, String> {
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let target = installed_exe();
    fs::create_dir_all(data_dir()).map_err(|e| e.to_string())?;
    if !same_file(&current, &target) {
        fs::copy(&current, &target).map_err(|e| format!("Не удалось скопировать программу: {e}"))?;
    }

    let t = ps_quote(&target.to_string_lossy());
    let w = ps_quote(&data_dir().to_string_lossy());
    let script = format!(
        "$ErrorActionPreference='Stop'\n\
         $s = New-Object -ComObject WScript.Shell\n\
         $dirs = @([Environment]::GetFolderPath('Desktop'), [Environment]::GetFolderPath('Programs'))\n\
         foreach ($d in $dirs) {{\n\
           $l = $s.CreateShortcut((Join-Path $d '{LINK_NAME}'))\n\
           $l.TargetPath = {t}\n\
           $l.WorkingDirectory = {w}\n\
           $l.IconLocation = {t} + ',0'\n\
           $l.Description = 'Экран телефона по Wi-Fi'\n\
           $l.Save()\n\
         }}\n"
    );
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand"])
        .arg(encode_command(&script))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::adb::hidden(&mut cmd);
    let out = cmd.output().map_err(|e| format!("Не удалось запустить PowerShell: {e}"))?;
    if !out.status.success() {
        return Err(format!("Ярлык не создан: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(format!("Ярлык «scrcpy Wi-Fi» добавлен на рабочий стол и в «Пуск» → {}", target.display()))
}

fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// PowerShell -EncodedCommand: base64 of UTF-16LE.
fn encode_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}
