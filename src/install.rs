//! Downloads the pinned official scrcpy release once and verifies its SHA-256.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// https://github.com/Genymobile/scrcpy/releases/tag/v5.0.1
pub const VERSION: &str = "v5.0.1";

#[cfg(target_arch = "x86")]
const BUILD: (&str, &str) = ("win32", "0cb65ebf1bce892fa4d1502d699af0c7a1a7f24d7febcb9ef72894e3f23fc2ad");
#[cfg(target_arch = "aarch64")]
const BUILD: (&str, &str) = ("winarm64", "16c3fc2068df64946f670e3504b4752073ebae47a416a45b46e79a5ab19c689b");
#[cfg(not(any(target_arch = "x86", target_arch = "aarch64")))]
const BUILD: (&str, &str) = ("win64", "b12a2c4ee8be317422451fc7dcf8ee20a71b5ea7ef9ad73ddd825a25316227a5");

pub fn data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("scrcpy-wifi")
}

pub fn scrcpy_dir() -> PathBuf {
    data_dir().join(format!("scrcpy-{VERSION}"))
}

pub fn is_installed() -> bool {
    let d = scrcpy_dir();
    ["scrcpy.exe", "adb.exe", "scrcpy-server"].iter().all(|f| d.join(f).is_file())
}

pub fn install(progress: impl Fn(String)) -> Result<PathBuf, String> {
    let (build, sha) = BUILD;
    let name = format!("scrcpy-{build}-{VERSION}");
    let url = format!("https://github.com/Genymobile/scrcpy/releases/download/{VERSION}/{name}.zip");
    progress(format!("Скачиваю {url}"));

    let agent = ureq::AgentBuilder::new()
        .try_proxy_from_env(true)
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .build();
    let resp = agent
        .get(&url)
        .call()
        .map_err(|e| format!("Не удалось скачать scrcpy: {e}"))?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(64 * 1024 * 1024)
        .read_to_end(&mut buf)
        .map_err(|e| format!("Обрыв при скачивании scrcpy: {e}"))?;

    let got = format!("{:x}", Sha256::digest(&buf));
    if got != sha {
        return Err(format!(
            "Контрольная сумма scrcpy не совпала, архив не использую.\nожидалось {sha}\nполучено  {got}"
        ));
    }
    progress(format!("Скачано {} КБ, контрольная сумма совпала", buf.len() / 1024));

    let final_dir = scrcpy_dir();
    let tmp = data_dir().join(format!(".tmp-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| format!("Не удалось создать {}: {e}", tmp.display()))?;
    if let Err(e) = extract(&buf, &tmp) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!("Не удалось распаковать scrcpy: {e}"));
    }
    let _ = fs::remove_dir_all(&final_dir);
    fs::rename(&tmp, &final_dir).map_err(|e| format!("Не удалось сохранить scrcpy: {e}"))?;
    progress(format!("scrcpy установлен: {}", final_dir.display()));
    Ok(final_dir)
}

/// Extracts the archive, dropping its top-level `scrcpy-<build>-<ver>/` folder.
fn extract(buf: &[u8], dest: &Path) -> io::Result<()> {
    let mut zip = zip::ZipArchive::new(io::Cursor::new(buf)).map_err(io::Error::other)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(io::Error::other)?;
        let Some(path) = entry.enclosed_name() else { continue };
        let rel: PathBuf = path.components().skip(1).collect();
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = fs::File::create(&out)?;
            io::copy(&mut entry, &mut file)?;
        }
    }
    Ok(())
}
