//! The accessibility app matrix: real browsers on the web fixtures and native editors, measured
//! through the public API (`tests/apps/`, contract in `tests/fixtures/apps/PROTOCOL.md`).
//!
//! One ignored test per OS family, run by `.github/workflows/a11y-apps.yml` with
//! `--ignored --nocapture --test-threads=1`. Configuration by environment (see `apps/mod.rs`):
//! `NUGGET_A11Y_APPS`, `NUGGET_A11Y_APPS_EDITORS`, `NUGGET_A11Y_MATRIX_OUT`,
//! `NUGGET_A11Y_APPS_FIXTURES`, `NUGGET_A11Y_APPS_FORCE_A11Y`.
//!
//! The non-ignored unit tests of the harness (HTTP server, scoring, the gate) run everywhere.

mod apps;

#[cfg(target_os = "macos")]
#[test]
#[ignore = "drives real apps; needs a macOS desktop session and Accessibility trust; run by a11y-apps.yml"]
fn apps_matrix_macos() {
    apps::run_matrix();
}

#[cfg(windows)]
#[test]
#[ignore = "drives real apps; needs an interactive Windows desktop; run by a11y-apps.yml"]
fn apps_matrix_windows() {
    apps::run_matrix();
}

/// On Linux the matrix runs in its own X session: Xvfb, a private session bus (the AT-SPI bus is
/// D-Bus activated on it) and a window manager when one is installed, then this test binary is
/// re-run inside it. With `NUGGET_A11Y_APPS_INNER` set, or `DISPLAY` set (a desktop session)
/// and `NUGGET_A11Y_APPS_XVFB` not `1`, it runs in the current session instead.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "drives real apps under Xvfb + AT-SPI; needs xvfb, dbus, at-spi2-core; run by a11y-apps.yml"]
fn apps_matrix_linux() {
    let inner = std::env::var_os(linux::INNER).is_some();
    let desktop = std::env::var_os("DISPLAY").is_some()
        && !std::env::var("NUGGET_A11Y_APPS_XVFB").is_ok_and(|v| v == "1");
    if inner || desktop {
        apps::run_matrix();
    } else {
        linux::run_in_session("apps_matrix_linux");
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use super::apps::launch::which;

    pub const INNER: &str = "NUGGET_A11Y_APPS_INNER";

    /// Kills its child on drop.
    struct Proc(Child);

    impl Drop for Proc {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    pub fn run_in_session(test: &str) {
        let display = (90..140)
            .find(|n| {
                !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
                    && !Path::new(&format!("/tmp/.X{n}-lock")).exists()
            })
            .expect("a free X display number");
        let xvfb = which(&["Xvfb"]).expect("harness cannot start: Xvfb (package xvfb)");
        let _x = Proc(
            Command::new(xvfb)
                .args([
                    format!(":{display}").as_str(),
                    "-screen",
                    "0",
                    "1600x1200x24",
                    "-nolisten",
                    "tcp",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start Xvfb"),
        );
        let sock = PathBuf::from(format!("/tmp/.X11-unix/X{display}"));
        let deadline = Instant::now() + Duration::from_secs(15);
        while !sock.exists() {
            assert!(
                Instant::now() < deadline,
                "harness cannot start: Xvfb did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        let dbus =
            which(&["dbus-daemon"]).expect("harness cannot start: dbus-daemon (package dbus)");
        let mut bus = Proc(
            Command::new(dbus)
                .args(["--session", "--nofork", "--print-address=1"])
                .env("DISPLAY", format!(":{display}"))
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("start dbus-daemon"),
        );
        let mut line = String::new();
        BufReader::new(bus.0.stdout.take().expect("dbus stdout"))
            .read_line(&mut line)
            .expect("bus address");
        let address = line.trim().to_string();
        assert!(
            !address.is_empty(),
            "harness cannot start: dbus-daemon printed no address"
        );

        // A window manager, so windows get focus and the `active` state.
        let wm = which(&["openbox", "xfwm4", "metacity"]).map(|wm| {
            eprintln!("window manager: {}", wm.display());
            Proc(
                Command::new(wm)
                    .env("DISPLAY", format!(":{display}"))
                    .env("DBUS_SESSION_BUS_ADDRESS", &address)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("start the window manager"),
            )
        });
        if wm.is_none() {
            eprintln!("no window manager installed (openbox): focus may not follow new windows");
        }
        std::thread::sleep(Duration::from_millis(500));

        let exe = std::env::current_exe().expect("test binary");
        let status = Command::new(exe)
            .args([
                "--exact",
                test,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(INNER, "1")
            .env("DISPLAY", format!(":{display}"))
            .env("DBUS_SESSION_BUS_ADDRESS", &address)
            .env("GTK_MODULES", "gail:atk-bridge")
            .env("NO_AT_BRIDGE", "0")
            .env("XDG_SESSION_TYPE", "x11")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("AT_SPI_BUS_ADDRESS")
            .status()
            .expect("run the matrix inside the X session");
        drop(wm);
        drop(bus);
        assert!(
            status.success(),
            "the apps matrix failed (see the output above)"
        );
    }
}
