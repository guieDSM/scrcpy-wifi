//! Thin wrapper over adb.exe from the scrcpy bundle.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Do not flash a console window for child processes.
pub fn hidden(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceKind {
    Connect,
    Pairing,
}

/// A phone announced on the local network via mDNS.
#[derive(Clone, Debug)]
pub struct Service {
    pub name: String,
    pub kind: ServiceKind,
    pub addr: String,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub serial: String,
    pub state: String,
    pub model: String,
}

impl Device {
    pub fn online(&self) -> bool {
        self.state == "device"
    }

    pub fn is_wifi(&self) -> bool {
        self.serial.contains("._adb-tls-connect")
            || self
                .serial
                .rsplit_once(':')
                .is_some_and(|(_, port)| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()))
    }

    /// mDNS instance name for serials like `adb-XXXX._adb-tls-connect._tcp`.
    pub fn mdns_name(&self) -> Option<&str> {
        self.serial.split_once("._adb-tls-connect").map(|(name, _)| name)
    }
}

#[derive(Clone)]
pub struct Adb {
    exe: PathBuf,
}

impl Adb {
    pub fn new(dir: &Path) -> Self {
        Self { exe: dir.join("adb.exe") }
    }

    /// Runs adb and returns (exit ok, stdout + stderr).
    pub fn run(&self, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(&self.exe);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hidden(&mut cmd);
        match cmd.output() {
            Ok(out) => {
                let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                (out.status.success(), text.trim().to_string())
            }
            Err(e) => (false, format!("adb: {e}")),
        }
    }

    pub fn mdns_services(&self) -> Vec<Service> {
        let (_, out) = self.run(&["mdns", "services"]);
        out.lines()
            .filter_map(|line| {
                let f: Vec<&str> = line.split_whitespace().collect();
                if f.len() < 3 {
                    return None;
                }
                let kind = if f[1].starts_with("_adb-tls-pairing") {
                    ServiceKind::Pairing
                } else if f[1].starts_with("_adb-tls-connect") {
                    ServiceKind::Connect
                } else {
                    return None;
                };
                Some(Service { name: f[0].to_string(), kind, addr: f[2].to_string() })
            })
            .collect()
    }

    pub fn devices(&self) -> Vec<Device> {
        let (_, out) = self.run(&["devices", "-l"]);
        out.lines()
            .skip_while(|l| !l.starts_with("List of devices"))
            .skip(1)
            .filter_map(|line| {
                let mut f = line.split_whitespace();
                let serial = f.next()?.to_string();
                let state = f.next()?.to_string();
                let model = f
                    .find_map(|t| t.strip_prefix("model:"))
                    .unwrap_or("Android")
                    .replace('_', " ");
                Some(Device { serial, state, model })
            })
            .collect()
    }

    pub fn connect(&self, addr: &str) -> String {
        self.run(&["connect", addr]).1
    }

    /// Returns (paired, adb output).
    pub fn pair(&self, addr: &str, code: &str) -> (bool, String) {
        let (_, out) = self.run(&["pair", addr, code]);
        (out.contains("Successfully paired"), out)
    }
}
