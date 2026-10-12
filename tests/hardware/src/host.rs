//! Host OS capture for the evidence record (§7.9 "host OS/build").

use std::process::Command;

/// The host the suite ran on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInfo {
    /// OS family as the register spells it: `macos` or `windows`.
    pub os: String,
    /// OS version and build, or `unknown` when the host does not say.
    pub build: String,
}

impl HostInfo {
    /// Capture the current host.
    #[must_use]
    pub fn capture() -> Self {
        let os = std::env::consts::OS.to_owned();
        let build = match os.as_str() {
            "macos" => {
                let version = run("sw_vers", &["-productVersion"]);
                let build = run("sw_vers", &["-buildVersion"]);
                match (version, build) {
                    (Some(version), Some(build)) => Some(format!("{version} ({build})")),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                }
            }
            "windows" => run("cmd", &["/C", "ver"]),
            _ => run("uname", &["-sr"]),
        };
        Self {
            os,
            build: build.unwrap_or_else(|| "unknown".to_owned()),
        }
    }
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}
