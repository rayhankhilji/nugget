//! User-level service installation: a launchd LaunchAgent on macOS, a systemd **user** unit on
//! Linux, and a Task Scheduler logon task on Windows. Nothing here needs or accepts root.
//!
//! The file generators are pure and compile everywhere (they are unit-tested on every OS); only
//! [`install`] and [`uninstall`] touch the system, and only on their own OS.

use std::path::{Path, PathBuf};

use nugget_core::{Error, Result};

/// launchd label and plist file stem.
pub const LAUNCHD_LABEL: &str = "dev.nugget.nuggetd";
/// systemd user unit name.
pub const SYSTEMD_UNIT: &str = "nuggetd.service";
/// Task Scheduler task name (root folder, which standard users may write).
pub const WINDOWS_TASK: &str = "Nugget Daemon";

const DESCRIPTION: &str = "Nugget background daemon (nuggetd)";
const DOCS_URL: &str = "https://github.com/rayhankhilji/nugget";

/// What the service runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    /// Absolute path of the `nuggetd` binary.
    pub exe: PathBuf,
    /// Data root passed as `--home` (set when the installer ran with `--home` or `NUGGET_HOME`).
    pub home: Option<PathBuf>,
    /// Where launchd writes the daemon's stdout/stderr (startup errors only; logs go to files).
    pub log_dir: PathBuf,
}

impl ServiceSpec {
    /// The daemon's arguments (no subcommand = run in the foreground).
    pub fn args(&self) -> Vec<String> {
        match &self.home {
            Some(h) => vec!["--home".into(), h.display().to_string()],
            None => Vec::new(),
        }
    }

    fn check(&self) -> Result<()> {
        let all = std::iter::once(self.exe.as_path())
            .chain(self.home.as_deref())
            .chain(std::iter::once(self.log_dir.as_path()));
        for p in all {
            if !p.is_absolute() {
                return Err(Error::Invalid(format!(
                    "service paths must be absolute: {}",
                    p.display()
                )));
            }
            if p.display().to_string().chars().any(char::is_control) {
                return Err(Error::Invalid(format!(
                    "service paths must not contain control characters: {:?}",
                    p
                )));
            }
        }
        Ok(())
    }
}

/// `~/Library/LaunchAgents/dev.nugget.nuggetd.plist` contents.
pub fn launchd_plist(spec: &ServiceSpec) -> String {
    let mut args = format!(
        "        <string>{}</string>\n",
        xml_escape(&spec.exe.display().to_string())
    );
    for a in spec.args() {
        args.push_str(&format!("        <string>{}</string>\n", xml_escape(&a)));
    }
    let log = xml_escape(&spec.log_dir.join("launchd.log").display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{args}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#
    )
}

/// `~/.config/systemd/user/nuggetd.service` contents.
pub fn systemd_unit(spec: &ServiceSpec) -> String {
    let mut exec = systemd_quote(&spec.exe.display().to_string());
    for a in spec.args() {
        exec.push(' ');
        exec.push_str(&systemd_quote(&a));
    }
    format!(
        "[Unit]
Description={DESCRIPTION}
Documentation={DOCS_URL}

[Service]
Type=simple
ExecStart={exec}
Restart=on-failure
RestartSec=5
KillSignal=SIGTERM
TimeoutStopSec=15
NoNewPrivileges=yes

[Install]
WantedBy=default.target
"
    )
}

/// Task Scheduler XML for a logon task of `user` (`DOMAIN\name`), least privilege.
pub fn windows_task_xml(spec: &ServiceSpec, user: &str) -> String {
    let user = xml_escape(user);
    let command = xml_escape(&spec.exe.display().to_string());
    let args = xml_escape(
        &spec
            .args()
            .iter()
            .map(|a| windows_quote(a))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let arguments = if args.is_empty() {
        String::new()
    } else {
        format!("\n      <Arguments>{args}</Arguments>")
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>{DESCRIPTION}</Description>
    <URI>\{WINDOWS_TASK}</URI>
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
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{command}</Command>{arguments}
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// UTF-16LE with a byte-order mark, the encoding `schtasks /XML` expects.
pub fn utf16le_with_bom(s: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for unit in s.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// One systemd command-line word: double-quoted, with `\`, `"`, `%` (specifiers) and `$`
/// (variable expansion) escaped.
fn systemd_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One Windows command-line argument (MSVC `CommandLineToArgvW` rules).
fn windows_quote(s: &str) -> String {
    if !s.is_empty() && !s.chars().any(|c| matches!(c, ' ' | '\t' | '"')) {
        return s.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in s.chars() {
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

/// Install and start the service for the current user. Returns a human-readable summary.
pub fn install(spec: &ServiceSpec) -> Result<String> {
    spec.check()?;
    imp::install(spec)
}

/// Stop and remove the service. Succeeds if it was not installed.
pub fn uninstall() -> Result<String> {
    imp::uninstall()
}

/// Run a command, returning stdout; a non-zero exit is an error carrying stderr.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
fn run(program: &str, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| Error::Other(format!("cannot run {program}: {e}")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        Err(Error::Other(format!(
            "`{program} {}` failed ({}): {}",
            args.join(" "),
            out.status,
            if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            }
        )))
    }
}

/// Refuse to install a per-user service as root.
#[cfg(unix)]
fn current_uid() -> Result<String> {
    Ok(run("id", &["-u"])?.trim().to_string())
}

#[cfg(unix)]
fn refuse_root() -> Result<String> {
    let uid = current_uid()?;
    if uid == "0" {
        return Err(Error::Invalid(
            "nuggetd installs a per-user service; run this as your normal user, not root".into(),
        ));
    }
    Ok(uid)
}

#[cfg(unix)]
fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| Error::Other("HOME is not set".into()))
}

#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
fn write_file(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, contents)?;
    Ok(())
}

#[cfg_attr(not(unix), allow(dead_code))]
fn remove_file_if_exists(path: &Path) -> Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    fn plist_path() -> Result<PathBuf> {
        Ok(home_dir()?
            .join("Library/LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist")))
    }

    pub(super) fn install(spec: &ServiceSpec) -> Result<String> {
        let uid = refuse_root()?;
        let path = plist_path()?;
        std::fs::create_dir_all(&spec.log_dir)?;
        // Replace a previous version: unload it first (fails harmlessly if not loaded).
        let _ = run(
            "launchctl",
            &["bootout", &format!("gui/{uid}/{LAUNCHD_LABEL}")],
        );
        write_file(&path, launchd_plist(spec).as_bytes())?;
        let path_str = path.display().to_string();
        run(
            "launchctl",
            &["bootstrap", &format!("gui/{uid}"), &path_str],
        )?;
        Ok(format!(
            "installed LaunchAgent {LAUNCHD_LABEL} ({path_str}); nuggetd now starts at login"
        ))
    }

    pub(super) fn uninstall() -> Result<String> {
        let uid = refuse_root()?;
        let path = plist_path()?;
        let _ = run(
            "launchctl",
            &["bootout", &format!("gui/{uid}/{LAUNCHD_LABEL}")],
        );
        Ok(if remove_file_if_exists(&path)? {
            format!("removed LaunchAgent {LAUNCHD_LABEL} ({})", path.display())
        } else {
            format!("LaunchAgent {LAUNCHD_LABEL} was not installed")
        })
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use super::*;

    fn unit_path() -> Result<PathBuf> {
        let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            Some(v) => PathBuf::from(v),
            None => home_dir()?.join(".config"),
        };
        Ok(base.join("systemd/user").join(SYSTEMD_UNIT))
    }

    pub(super) fn install(spec: &ServiceSpec) -> Result<String> {
        refuse_root()?;
        let path = unit_path()?;
        write_file(&path, systemd_unit(spec).as_bytes())?;
        let enable = run("systemctl", &["--user", "daemon-reload"])
            .and_then(|_| run("systemctl", &["--user", "enable", "--now", SYSTEMD_UNIT]));
        match enable {
            Ok(_) => Ok(format!(
                "installed systemd user unit {SYSTEMD_UNIT} ({}); nuggetd now starts at login",
                path.display()
            )),
            Err(e) => Err(Error::Other(format!(
                "wrote {} but could not enable it ({e}); run `systemctl --user enable --now {SYSTEMD_UNIT}` in your login session",
                path.display()
            ))),
        }
    }

    pub(super) fn uninstall() -> Result<String> {
        refuse_root()?;
        let path = unit_path()?;
        let _ = run("systemctl", &["--user", "disable", "--now", SYSTEMD_UNIT]);
        let removed = remove_file_if_exists(&path)?;
        let _ = run("systemctl", &["--user", "daemon-reload"]);
        Ok(if removed {
            format!(
                "removed systemd user unit {SYSTEMD_UNIT} ({})",
                path.display()
            )
        } else {
            format!("systemd user unit {SYSTEMD_UNIT} was not installed")
        })
    }
}

#[cfg(windows)]
mod imp {
    use super::*;

    fn current_user() -> Result<String> {
        let name =
            std::env::var("USERNAME").map_err(|_| Error::Other("USERNAME is not set".into()))?;
        Ok(match std::env::var("USERDOMAIN") {
            Ok(d) if !d.is_empty() => format!("{d}\\{name}"),
            _ => name,
        })
    }

    pub(super) fn install(spec: &ServiceSpec) -> Result<String> {
        let user = current_user()?;
        let xml = windows_task_xml(spec, &user);
        let tmp = std::env::temp_dir().join(format!("nuggetd-task-{}.xml", std::process::id()));
        write_file(&tmp, &utf16le_with_bom(&xml))?;
        let tmp_str = tmp.display().to_string();
        let created = run(
            "schtasks",
            &["/Create", "/TN", WINDOWS_TASK, "/XML", &tmp_str, "/F"],
        );
        let _ = std::fs::remove_file(&tmp);
        created?;
        // Start it now as well as at the next logon.
        if let Err(e) = run("schtasks", &["/Run", "/TN", WINDOWS_TASK]) {
            tracing::warn!(error = %e, "task created but could not be started now");
        }
        Ok(format!(
            "installed scheduled task \"{WINDOWS_TASK}\" for {user}; nuggetd now starts at logon"
        ))
    }

    pub(super) fn uninstall() -> Result<String> {
        let _ = run("schtasks", &["/End", "/TN", WINDOWS_TASK]);
        match run("schtasks", &["/Delete", "/TN", WINDOWS_TASK, "/F"]) {
            Ok(_) => Ok(format!("removed scheduled task \"{WINDOWS_TASK}\"")),
            Err(e) => {
                // Not installed is not an error.
                if run("schtasks", &["/Query", "/TN", WINDOWS_TASK]).is_err() {
                    Ok(format!(
                        "scheduled task \"{WINDOWS_TASK}\" was not installed"
                    ))
                } else {
                    Err(e)
                }
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::*;

    pub(super) fn install(_spec: &ServiceSpec) -> Result<String> {
        Err(Error::Unsupported(
            "service installation is not supported on this OS".into(),
        ))
    }

    pub(super) fn uninstall() -> Result<String> {
        Err(Error::Unsupported(
            "service installation is not supported on this OS".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(home: Option<&str>) -> ServiceSpec {
        ServiceSpec {
            exe: PathBuf::from("/opt/Nugget & Co/bin/nuggetd"),
            home: home.map(PathBuf::from),
            log_dir: PathBuf::from("/Users/me/Library/Application Support/Nugget/logs"),
        }
    }

    #[test]
    fn launchd_plist_runs_at_load_and_restarts_on_crash() {
        let p = launchd_plist(&spec(Some("/tmp/n<1>")));
        assert!(p.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"));
        assert!(p.contains("<key>Label</key>\n    <string>dev.nugget.nuggetd</string>"));
        assert!(p.contains(
            "    <array>\n        <string>/opt/Nugget &amp; Co/bin/nuggetd</string>\n        <string>--home</string>\n        <string>/tmp/n&lt;1&gt;</string>\n    </array>"
        ));
        assert!(p.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(p.contains("<key>SuccessfulExit</key>\n        <false/>"));
        // The log path is joined with the host's separator (launchd only runs on macOS).
        let log = spec(None).log_dir.join("launchd.log");
        assert!(
            p.contains(&format!("<string>{}</string>", log.display())),
            "{p}"
        );
        // A user agent: no root-only keys.
        assert!(!p.contains("UserName"));
        assert!(!p.contains("GroupName"));
        assert_eq!(p.matches("<dict>").count(), p.matches("</dict>").count());
    }

    #[test]
    fn launchd_plist_without_home_has_only_the_binary() {
        let p = launchd_plist(&spec(None));
        assert!(!p.contains("--home"));
        assert_eq!(
            p.matches("<string>/opt/Nugget &amp; Co/bin/nuggetd</string>")
                .count(),
            1
        );
    }

    #[test]
    fn systemd_unit_is_a_user_unit_with_quoted_exec() {
        let u = systemd_unit(&spec(Some("/home/me/100%$HOME")));
        assert!(u.starts_with("[Unit]\nDescription=Nugget background daemon (nuggetd)\n"));
        assert!(u.contains(
            "\nExecStart=\"/opt/Nugget & Co/bin/nuggetd\" \"--home\" \"/home/me/100%%$$HOME\"\n"
        ));
        assert!(u.contains("\nRestart=on-failure\n"));
        assert!(u.contains("\nNoNewPrivileges=yes\n"));
        assert!(u.ends_with("[Install]\nWantedBy=default.target\n"));
        // User units must not switch users.
        assert!(!u.contains("User="));
        let plain = systemd_unit(&spec(None));
        assert!(plain.contains("\nExecStart=\"/opt/Nugget & Co/bin/nuggetd\"\n"));
    }

    #[test]
    fn windows_task_is_a_least_privilege_logon_task() {
        let s = ServiceSpec {
            exe: PathBuf::from(r"C:\Program Files\Nugget\nuggetd.exe"),
            home: Some(PathBuf::from(r"D:\Nugget Data\")),
            log_dir: PathBuf::from(r"C:\logs"),
        };
        let x = windows_task_xml(&s, r"WORK\R&D");
        assert!(
            x.starts_with("<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<Task version=\"1.2\"")
        );
        assert!(x.contains(
            "<LogonTrigger>\n      <Enabled>true</Enabled>\n      <UserId>WORK\\R&amp;D</UserId>"
        ));
        assert!(x.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(x.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(!x.contains("HighestAvailable"));
        assert!(x.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(x.contains(r"<Command>C:\Program Files\Nugget\nuggetd.exe</Command>"));
        // Trailing backslash doubled so it does not escape the closing quote.
        assert!(x.contains(r#"<Arguments>--home &quot;D:\Nugget Data\\&quot;</Arguments>"#));
        assert!(x.contains(r"<URI>\Nugget Daemon</URI>"));

        let no_home = ServiceSpec { home: None, ..s };
        assert!(!windows_task_xml(&no_home, "me").contains("<Arguments>"));
    }

    #[test]
    fn utf16_encoding_has_bom() {
        assert_eq!(utf16le_with_bom("A<"), vec![0xFF, 0xFE, b'A', 0, b'<', 0]);
    }

    #[test]
    fn windows_quoting_follows_argv_rules() {
        assert_eq!(windows_quote("plain"), "plain");
        assert_eq!(windows_quote("a b"), "\"a b\"");
        assert_eq!(windows_quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(windows_quote(r"C:\dir\"), r"C:\dir\");
        assert_eq!(windows_quote(r"C:\my dir\"), r#""C:\my dir\\""#);
        assert_eq!(windows_quote(""), "\"\"");
    }

    #[test]
    fn rejects_relative_or_control_char_paths() {
        let mut s = spec(None);
        s.exe = PathBuf::from("nuggetd");
        assert!(s.check().is_err());
        let mut s = spec(Some("/tmp/a\nb"));
        s.exe = PathBuf::from("/bin/nuggetd");
        assert!(s.check().is_err());
        assert!(spec(Some("/tmp/x")).check().is_ok() || cfg!(windows));
    }
}
