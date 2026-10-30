//! A desktop entry named after the app id, for AppImage runs on Linux.
//!
//! The XDG desktop portal only lets an unsandboxed app name itself (`host.portal.Registry`) when
//! GLib, in the portal's own environment, can load `dev.nugget.desktop.desktop`: the file must
//! sit in `$XDG_DATA_HOME/applications` or `$XDG_DATA_DIRS/*/applications`, and its `Exec`
//! program (and `TryExec`, if any) must exist. Without that, `GlobalShortcuts` refuses the
//! session. The .deb and .rpm install that file under `/usr/share/applications`; an AppImage's
//! own copy is inside its image, where the portal cannot see it. So an AppImage run writes a
//! hidden (`NoDisplay=true`) entry pointing at the AppImage file into the user's data dir.
//!
//! GLib takes the first `dev.nugget.desktop.desktop` it finds in precedence order (the user's
//! data dir, then each system data dir) and does not fall back when that one fails to load, so
//! the decision below looks at the first match only. A file is rewritten only if it carries
//! [`MARKER`], i.e. an earlier run wrote it; anything else is left alone.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The desktop file id: the app id (the Tauri `identifier`) plus `.desktop`.
const FILE_NAME: &str = "dev.nugget.desktop.desktop";

/// Key that marks a file this module wrote, so it may be rewritten later.
const MARKER: &str = "X-Nugget-Generated";

/// The icon name the Tauri bundles install (`/usr/share/icons/hicolor/*/apps/<name>.png`, named
/// after the main binary).
const ICON: &str = "nugget-desktop";

/// Tauri's own desktop file sets `StartupWMClass` to the main binary's name.
const WM_CLASS: &str = "nugget-desktop";

/// GLib's search path when `PATH` is unset.
const DEFAULT_PATH: &str = "/bin:/usr/bin:.";

/// The environment the decision depends on, read once so tests can supply their own.
#[derive(Debug, Default, Clone)]
struct Env {
    appimage: Option<OsString>,
    home: Option<OsString>,
    xdg_data_home: Option<OsString>,
    xdg_data_dirs: Option<OsString>,
    path: Option<OsString>,
}

impl Env {
    fn from_process() -> Self {
        Self {
            appimage: std::env::var_os("APPIMAGE"),
            home: std::env::var_os("HOME"),
            xdg_data_home: std::env::var_os("XDG_DATA_HOME"),
            xdg_data_dirs: std::env::var_os("XDG_DATA_DIRS"),
            path: std::env::var_os("PATH"),
        }
    }
}

/// When running from an AppImage and no loadable `dev.nugget.desktop.desktop` is visible in the
/// XDG data dirs, write one to `$XDG_DATA_HOME/applications` (default
/// `~/.local/share/applications`). Returns the path written, or `None` when nothing was needed
/// (not an AppImage, a usable entry already exists, or a file we did not write is in the way).
pub(crate) fn ensure_appimage_entry() -> io::Result<Option<PathBuf>> {
    ensure_with(&Env::from_process())
}

fn ensure_with(env: &Env) -> io::Result<Option<PathBuf>> {
    let Some(appimage) = env.appimage.as_deref().map(Path::new) else {
        return Ok(None);
    };
    if !appimage.is_absolute() || !appimage.is_file() {
        return Ok(None);
    }
    let Some(appimage_str) = appimage.to_str() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the AppImage path is not UTF-8, which a desktop entry cannot hold",
        ));
    };
    let Some(rendered) = render_entry(appimage_str) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the AppImage path holds '%' or a control character, which a desktop entry's Exec \
             cannot express unambiguously",
        ));
    };
    let Some((data_home, data_dirs)) = search_dirs(
        env.xdg_data_home.as_deref(),
        env.home.as_deref(),
        env.xdg_data_dirs.as_deref(),
    ) else {
        return Ok(None);
    };
    let target = data_home.join("applications").join(FILE_NAME);
    let dirs: Vec<PathBuf> = std::iter::once(data_home).chain(data_dirs).collect();
    let found = match find_entry(&dirs) {
        Some(path) => Some(Found::read(path, env.path.as_deref())?),
        None => None,
    };
    match plan(&target, found.as_ref(), &rendered) {
        Plan::Nothing => Ok(None),
        Plan::Foreign(path) => {
            tracing::warn!(
                path = %path.display(),
                "a desktop entry Nugget did not write is in the way and cannot be loaded; the \
                 desktop portal will not know this app's id"
            );
            Ok(None)
        }
        Plan::Write(path) => {
            write_atomic(&path, rendered.as_bytes())?;
            tracing::info!(path = %path.display(), "installed a desktop entry for the AppImage");
            Ok(Some(path))
        }
    }
}

/// The user's data dir and the system data dirs, per the XDG base directory spec. Relative
/// entries are ignored, as the spec requires. `None` when there is no user data dir at all.
fn search_dirs(
    xdg_data_home: Option<&OsStr>,
    home: Option<&OsStr>,
    xdg_data_dirs: Option<&OsStr>,
) -> Option<(PathBuf, Vec<PathBuf>)> {
    let data_home = match xdg_data_home.map(Path::new) {
        Some(p) if p.is_absolute() => p.to_path_buf(),
        _ => {
            let home = Path::new(home.filter(|h| !h.is_empty())?);
            if !home.is_absolute() {
                return None;
            }
            home.join(".local/share")
        }
    };
    let dirs_env = xdg_data_dirs.filter(|d| !d.is_empty());
    let dirs = std::env::split_paths(dirs_env.unwrap_or(OsStr::new("/usr/local/share:/usr/share")))
        .filter(|p| p.is_absolute())
        .collect();
    Some((data_home, dirs))
}

/// The first `<dir>/applications/dev.nugget.desktop.desktop` that exists, in the given order.
fn find_entry(dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|d| d.join("applications").join(FILE_NAME))
        .find(|p| p.is_file())
}

/// A desktop file found on the search path.
#[derive(Debug)]
struct Found {
    path: PathBuf,
    contents: String,
    generated: bool,
    usable: bool,
}

impl Found {
    fn read(path: PathBuf, path_env: Option<&OsStr>) -> io::Result<Self> {
        // A file that is not UTF-8 does not load in GLib either: treat it as unusable, foreign.
        let contents = match fs::read(&path) {
            Ok(bytes) => String::from_utf8(bytes).unwrap_or_default(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let entry = parse_entry(&contents);
        Ok(Self {
            path,
            generated: entry.generated,
            usable: entry.usable(path_env),
            contents,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Plan {
    /// A usable entry is already visible (or ours is up to date).
    Nothing,
    /// The first entry is unusable but not ours to rewrite.
    Foreign(PathBuf),
    /// Write `rendered` here.
    Write(PathBuf),
}

/// What to do, given where we would write (`target`), the first entry GLib would load, and the
/// file we would write.
fn plan(target: &Path, found: Option<&Found>, rendered: &str) -> Plan {
    match found {
        None => Plan::Write(target.to_path_buf()),
        Some(f) if f.path == target => {
            if !f.generated {
                if f.usable {
                    Plan::Nothing
                } else {
                    Plan::Foreign(f.path.clone())
                }
            } else if f.contents == rendered {
                Plan::Nothing
            } else {
                // Ours, but for another (often deleted) AppImage path or an older layout.
                Plan::Write(target.to_path_buf())
            }
        }
        // In a system dir, which the user's data dir shadows: a file there takes precedence.
        Some(f) if f.usable => Plan::Nothing,
        Some(_) => Plan::Write(target.to_path_buf()),
    }
}

/// The keys of the `[Desktop Entry]` group that decide whether GLib loads the file.
#[derive(Debug, Default, PartialEq, Eq)]
struct Entry {
    type_: Option<String>,
    exec: Option<String>,
    try_exec: Option<String>,
    hidden: bool,
    generated: bool,
}

impl Entry {
    /// Whether `g_desktop_app_info_new` would load it: `Type=Application`, not `Hidden`, and
    /// `TryExec` and `Exec`'s program (when present) found.
    fn usable(&self, path_env: Option<&OsStr>) -> bool {
        if self.type_.as_deref() != Some("Application") || self.hidden {
            return false;
        }
        if let Some(t) = self.try_exec.as_deref().filter(|t| !t.is_empty())
            && !program_exists(t, path_env)
        {
            return false;
        }
        match self.exec.as_deref().filter(|e| !e.is_empty()) {
            None => true,
            Some(exec) => exec_program(exec).is_some_and(|p| program_exists(&p, path_env)),
        }
    }
}

/// Parse the keys [`Entry`] holds from the first `[Desktop Entry]` group. The first occurrence
/// of a key wins; values are unescaped as desktop-entry strings.
fn parse_entry(text: &str) -> Entry {
    let mut entry = Entry::default();
    let mut in_group = false;
    for line in text.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            if in_group {
                break;
            }
            in_group = line.trim_end() == "[Desktop Entry]";
            continue;
        }
        if !in_group {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = unescape_string(value.trim_start());
        let slot = match key.trim_end() {
            "Type" => &mut entry.type_,
            "Exec" => &mut entry.exec,
            "TryExec" => &mut entry.try_exec,
            "Hidden" => {
                entry.hidden = value.trim_end() == "true";
                continue;
            }
            MARKER => {
                entry.generated = value.trim_end() == "true";
                continue;
            }
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(value.trim_end().to_string());
        }
    }
    entry
}

/// Undo the desktop-entry string escapes `\s`, `\n`, `\t`, `\r` and `\\`. Other backslashes are
/// kept, as GLib does.
fn unescape_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The program (argv\[0]) of an unescaped `Exec` value, split the way GLib does it
/// (`g_shell_parse_argv`): double quotes with `\` escaping `"`, `` ` ``, `$` and `\`, single
/// quotes taken literally, and an unquoted `\` escaping the next character. `None` when the
/// value is empty or a quote is left open.
fn exec_program(exec: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = exec
        .trim_start_matches([' ', '\t', '\n'])
        .chars()
        .peekable();
    let mut started = false;
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => break,
            '\'' => {
                started = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        ch => out.push(ch),
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            ch @ ('"' | '`' | '$' | '\\') => out.push(ch),
                            '\n' => {}
                            ch => {
                                out.push('\\');
                                out.push(ch);
                            }
                        },
                        ch => out.push(ch),
                    }
                }
            }
            '\\' => {
                started = true;
                match chars.next() {
                    Some('\n') => {}
                    Some(ch) => out.push(ch),
                    None => out.push('\\'),
                }
            }
            ch => {
                started = true;
                out.push(ch);
            }
        }
    }
    (started && !out.is_empty()).then_some(out)
}

/// Whether `program` resolves as GLib's `g_find_program_for_path` resolves it: an absolute path
/// to an executable file, or a bare name found in `PATH`. A relative path with a `/` depends on
/// the portal's working directory, so it never counts.
fn program_exists(program: &str, path_env: Option<&OsStr>) -> bool {
    let p = Path::new(program);
    if p.is_absolute() {
        return is_executable(p);
    }
    if program.contains('/') {
        return false;
    }
    let path_env = path_env.unwrap_or(OsStr::new(DEFAULT_PATH));
    std::env::split_paths(path_env)
        .filter(|d| d.is_absolute())
        .any(|d| is_executable(&d.join(program)))
}

fn is_executable(p: &Path) -> bool {
    fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `arg` as one double-quoted `Exec` argument (desktop-entry quoting, then string escaping).
/// `None` for a `%` (a field code) or a control character, which do not round-trip through
/// every desktop-entry reader.
fn quote_exec_arg(arg: &str) -> Option<String> {
    if arg.is_empty() || arg.contains('%') || arg.chars().any(char::is_control) {
        return None;
    }
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    // The string escape is applied on top: every backslash doubles.
    Some(quoted.replace('\\', "\\\\"))
}

/// The desktop file for an AppImage at `appimage`, or `None` when its path cannot be quoted.
fn render_entry(appimage: &str) -> Option<String> {
    let exec = quote_exec_arg(appimage)?;
    Some(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Nugget\n\
         Comment=Lets the desktop portal identify Nugget run from an AppImage\n\
         Exec={exec}\n\
         Icon={ICON}\n\
         StartupWMClass={WM_CLASS}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         {MARKER}=true\n"
    ))
}

/// Write `contents` to `path` through a temporary file in the same dir and a rename, mode 0644.
fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent directory"))?;
    fs::create_dir_all(dir)?;
    let name = path.file_name().unwrap_or(OsStr::new(FILE_NAME));
    let mut tmp_name = OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = dir.join(tmp_name);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .open(&tmp)?;
        file.write_all(contents)?;
        // The umask may have narrowed the mode; desktop files are world-readable.
        file.set_permissions(fs::Permissions::from_mode(0o644))?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    struct Sandbox {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    impl Sandbox {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().to_path_buf();
            Self { _dir: dir, root }
        }

        fn env(&self, appimage: &Path) -> Env {
            Env {
                appimage: Some(appimage.into()),
                home: Some(self.root.join("home").into()),
                xdg_data_home: None,
                xdg_data_dirs: Some(
                    std::env::join_paths([self.root.join("sys1"), self.root.join("sys2")]).unwrap(),
                ),
                path: Some(self.root.join("bin").into()),
            }
        }

        fn user_file(&self) -> PathBuf {
            self.root
                .join("home/.local/share/applications")
                .join(FILE_NAME)
        }

        fn sys_file(&self, n: u8) -> PathBuf {
            self.root
                .join(format!("sys{n}/applications"))
                .join(FILE_NAME)
        }
    }

    fn put(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn unescape_handles_string_escapes() {
        assert_eq!(unescape_string(r"a\sb\tc\nd\\e\qf\"), "a b\tc\nd\\e\\qf\\");
    }

    #[test]
    fn exec_program_follows_shell_quoting() {
        assert_eq!(
            exec_program("nugget-desktop %u").as_deref(),
            Some("nugget-desktop")
        );
        assert_eq!(
            exec_program("  /usr/bin/x --a").as_deref(),
            Some("/usr/bin/x")
        );
        assert_eq!(
            exec_program(r#""/a b/c\"d\$e\`f\\g\h" x"#).as_deref(),
            Some(r#"/a b/c"d$e`f\g\h"#)
        );
        assert_eq!(exec_program("'/a b/$x' y").as_deref(), Some("/a b/$x"));
        assert_eq!(exec_program(r"/a\ b c").as_deref(), Some("/a b"));
        assert_eq!(exec_program(r#"/a"b c"d e"#).as_deref(), Some("/ab cd"));
        assert_eq!(exec_program(r#""/open"#), None);
        assert_eq!(exec_program("'/open"), None);
        assert_eq!(exec_program("   "), None);
        assert_eq!(exec_program(r#""""#), None);
    }

    #[test]
    fn quoting_round_trips_through_unescape_and_parse() {
        for path in [
            "/home/u/Apps/Nugget_0.0.1_amd64.AppImage",
            "/home/u/My Apps/Nugget.AppImage",
            r#"/tmp/we"ird/$HOME/`x`/back\slash/'q'/#;&|<>*?()~=.AppImage"#,
            "/tmp/ünïcödé/Nugget.AppImage",
        ] {
            let quoted = quote_exec_arg(path).unwrap();
            assert!(quoted.starts_with('"') && quoted.ends_with('"'), "{quoted}");
            let line = format!("{quoted} %U");
            assert_eq!(
                exec_program(&unescape_string(&line)).as_deref(),
                Some(path),
                "{line}"
            );
        }
        assert_eq!(quote_exec_arg(r"/a\b").as_deref(), Some(r#""/a\\\\b""#));
        assert_eq!(quote_exec_arg("/a%20b"), None);
        assert_eq!(quote_exec_arg("/a\nb"), None);
        assert_eq!(quote_exec_arg(""), None);
    }

    #[test]
    fn rendered_entry_parses_back() {
        let text = render_entry("/opt/My Apps/Nugget.AppImage").unwrap();
        assert!(
            text.contains("Exec=\"/opt/My Apps/Nugget.AppImage\"\n"),
            "{text}"
        );
        assert!(text.contains("NoDisplay=true\n"));
        assert!(text.contains("Icon=nugget-desktop\n"));
        let entry = parse_entry(&text);
        assert!(entry.generated);
        assert_eq!(entry.type_.as_deref(), Some("Application"));
        assert_eq!(
            exec_program(entry.exec.as_deref().unwrap()).as_deref(),
            Some("/opt/My Apps/Nugget.AppImage")
        );
        assert_eq!(render_entry("/a%b"), None);
    }

    #[test]
    fn parse_reads_only_the_desktop_entry_group() {
        let entry = parse_entry(
            "# c\n[Other]\nExec=/nope\n[Desktop Entry]\nType = Application\nExec= /bin/a\\sb\n\
             Exec=/second\nTryExec=a\nHidden=false\nX-Nugget-Generated=true\n\
             [Desktop Action new]\nHidden=true\n",
        );
        assert_eq!(
            entry,
            Entry {
                type_: Some("Application".into()),
                exec: Some("/bin/a b".into()),
                try_exec: Some("a".into()),
                hidden: false,
                generated: true,
            }
        );
    }

    #[test]
    fn search_dirs_follows_the_base_dir_spec() {
        let (home, dirs) = search_dirs(None, Some(OsStr::new("/h")), None).unwrap();
        assert_eq!(home, Path::new("/h/.local/share"));
        assert_eq!(
            dirs,
            [Path::new("/usr/local/share"), Path::new("/usr/share")]
        );

        let (home, dirs) = search_dirs(
            Some(OsStr::new("/x/data")),
            Some(OsStr::new("/h")),
            Some(OsStr::new("/a:rel:/b:")),
        )
        .unwrap();
        assert_eq!(home, Path::new("/x/data"));
        assert_eq!(dirs, [Path::new("/a"), Path::new("/b")]);

        // A relative or empty XDG_DATA_HOME falls back to HOME; an empty XDG_DATA_DIRS to the
        // defaults.
        let (home, dirs) = search_dirs(
            Some(OsStr::new("rel")),
            Some(OsStr::new("/h")),
            Some(OsStr::new("")),
        )
        .unwrap();
        assert_eq!(home, Path::new("/h/.local/share"));
        assert_eq!(dirs.len(), 2);
        assert!(search_dirs(Some(OsStr::new("")), None, None).is_none());
        assert!(search_dirs(None, Some(OsStr::new("rel")), None).is_none());
    }

    #[test]
    fn program_lookup() {
        let sb = Sandbox::new();
        let bin = sb.root.join("bin");
        exe(&bin.join("tool"));
        put(&bin.join("plain"), "not executable");
        let path = std::env::join_paths([Path::new("rel"), &bin]).unwrap();
        assert!(program_exists("tool", Some(&path)));
        assert!(program_exists(bin.join("tool").to_str().unwrap(), None));
        assert!(!program_exists("plain", Some(&path)));
        assert!(!program_exists("missing", Some(&path)));
        assert!(!program_exists("bin/tool", Some(&path)));
        assert!(!program_exists(bin.to_str().unwrap(), Some(&path)));
    }

    #[test]
    fn usable_mirrors_glib() {
        let sb = Sandbox::new();
        exe(&sb.root.join("bin/app"));
        let path = sb.root.join("bin").into_os_string();
        let ok = |t: &str| parse_entry(t).usable(Some(&path));
        assert!(ok("[Desktop Entry]\nType=Application\nExec=app --x\n"));
        assert!(ok("[Desktop Entry]\nType=Application\n"));
        assert!(!ok("[Desktop Entry]\nType=Link\nExec=app\n"));
        assert!(!ok("[Desktop Entry]\nExec=app\n"));
        assert!(!ok("[Desktop Entry]\nType=Application\nExec=gone\n"));
        assert!(!ok(
            "[Desktop Entry]\nType=Application\nExec=app\nHidden=true\n"
        ));
        assert!(!ok(
            "[Desktop Entry]\nType=Application\nExec=app\nTryExec=gone\n"
        ));
        assert!(!ok("[Desktop Entry]\nType=Application\nExec=\"app\n"));
    }

    #[test]
    fn not_an_appimage_does_nothing() {
        let sb = Sandbox::new();
        let mut env = sb.env(&sb.root.join("missing.AppImage"));
        assert_eq!(ensure_with(&env).unwrap(), None);
        env.appimage = None;
        assert_eq!(ensure_with(&env).unwrap(), None);
        env.appimage = Some("relative.AppImage".into());
        assert_eq!(ensure_with(&env).unwrap(), None);
        assert!(!sb.user_file().exists());
    }

    #[test]
    fn writes_then_keeps_then_refreshes() {
        let sb = Sandbox::new();
        let first = sb.root.join("dl/Nugget one.AppImage");
        exe(&first);
        let written = ensure_with(&sb.env(&first)).unwrap();
        assert_eq!(written.as_deref(), Some(sb.user_file().as_path()));
        let text = fs::read_to_string(sb.user_file()).unwrap();
        assert_eq!(text, render_entry(first.to_str().unwrap()).unwrap());
        let mode = fs::metadata(sb.user_file()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
        // No temporary file left behind.
        let names: Vec<_> = fs::read_dir(sb.user_file().parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [OsString::from(FILE_NAME)]);

        // Up to date: nothing to do.
        assert_eq!(ensure_with(&sb.env(&first)).unwrap(), None);

        // The AppImage moved: our file is rewritten for the new path.
        let second = sb.root.join("dl/Nugget-2.AppImage");
        exe(&second);
        fs::remove_file(&first).unwrap();
        assert_eq!(
            ensure_with(&sb.env(&second)).unwrap().as_deref(),
            Some(sb.user_file().as_path())
        );
        let entry = parse_entry(&fs::read_to_string(sb.user_file()).unwrap());
        assert_eq!(
            exec_program(entry.exec.as_deref().unwrap()).as_deref(),
            second.to_str()
        );
    }

    #[test]
    fn honours_xdg_data_home() {
        let sb = Sandbox::new();
        let app = sb.root.join("Nugget.AppImage");
        exe(&app);
        let mut env = sb.env(&app);
        env.xdg_data_home = Some(sb.root.join("data").into());
        let want = sb.root.join("data/applications").join(FILE_NAME);
        assert_eq!(ensure_with(&env).unwrap(), Some(want));
        assert!(!sb.user_file().exists());
    }

    #[test]
    fn a_usable_system_entry_wins() {
        let sb = Sandbox::new();
        let app = sb.root.join("Nugget.AppImage");
        exe(&app);
        exe(&sb.root.join("bin/nugget-desktop"));
        put(
            &sb.sys_file(2),
            "[Desktop Entry]\nType=Application\nExec=nugget-desktop\n",
        );
        assert_eq!(ensure_with(&sb.env(&app)).unwrap(), None);
        assert!(!sb.user_file().exists());
    }

    #[test]
    fn a_broken_system_entry_is_shadowed() {
        let sb = Sandbox::new();
        let app = sb.root.join("Nugget.AppImage");
        exe(&app);
        // The first match decides (GLib does not fall back), even with a good one after it.
        put(
            &sb.sys_file(1),
            "[Desktop Entry]\nType=Application\nExec=/usr/bin/gone-for-good\n",
        );
        exe(&sb.root.join("bin/nugget-desktop"));
        put(
            &sb.sys_file(2),
            "[Desktop Entry]\nType=Application\nExec=nugget-desktop\n",
        );
        assert_eq!(ensure_with(&sb.env(&app)).unwrap(), Some(sb.user_file()));
        assert_eq!(
            fs::read_to_string(sb.sys_file(1)).unwrap().lines().count(),
            3
        );
    }

    #[test]
    fn never_overwrites_a_foreign_file() {
        let sb = Sandbox::new();
        let app = sb.root.join("Nugget.AppImage");
        exe(&app);
        let foreign = "[Desktop Entry]\nType=Application\nExec=/nowhere/nugget\nName=Mine\n";
        put(&sb.user_file(), foreign);
        assert_eq!(ensure_with(&sb.env(&app)).unwrap(), None);
        assert_eq!(fs::read_to_string(sb.user_file()).unwrap(), foreign);

        // Usable and foreign: also left alone.
        let usable = format!(
            "[Desktop Entry]\nType=Application\nExec=\"{}\"\n",
            app.display()
        );
        put(&sb.user_file(), &usable);
        assert_eq!(ensure_with(&sb.env(&app)).unwrap(), None);
        assert_eq!(fs::read_to_string(sb.user_file()).unwrap(), usable);
    }

    #[test]
    fn rejects_unrepresentable_paths() {
        let sb = Sandbox::new();
        let app = sb.root.join("100%.AppImage");
        exe(&app);
        let err = ensure_with(&sb.env(&app)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!sb.user_file().exists());
    }

    #[test]
    fn plan_cases() {
        let target = Path::new("/h/applications/x.desktop");
        let found = |path: &str, generated, usable, contents: &str| Found {
            path: path.into(),
            contents: contents.into(),
            generated,
            usable,
        };
        assert_eq!(plan(target, None, "r"), Plan::Write(target.into()));
        let t = target.to_str().unwrap();
        assert_eq!(
            plan(target, Some(&found(t, true, true, "r")), "r"),
            Plan::Nothing
        );
        assert_eq!(
            plan(target, Some(&found(t, true, true, "old")), "r"),
            Plan::Write(target.into())
        );
        assert_eq!(
            plan(target, Some(&found(t, true, false, "old")), "r"),
            Plan::Write(target.into())
        );
        assert_eq!(
            plan(target, Some(&found(t, false, true, "")), "r"),
            Plan::Nothing
        );
        assert_eq!(
            plan(target, Some(&found(t, false, false, "")), "r"),
            Plan::Foreign(target.into())
        );
        assert_eq!(
            plan(target, Some(&found("/usr/share/a/x", false, true, "")), "r"),
            Plan::Nothing
        );
        assert_eq!(
            plan(target, Some(&found("/usr/share/a/x", true, false, "")), "r"),
            Plan::Write(target.into())
        );
    }
}
