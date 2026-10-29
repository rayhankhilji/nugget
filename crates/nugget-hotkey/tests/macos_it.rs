//! macOS integration tests on a real desktop: the Double Tap listeners fed with posted `CGEvent`s,
//! and the trigger-to-insert path into TextEdit, read back through AX.
//!
//! NSEvent monitors and Carbon hot keys are delivered by the main thread's `NSApplication` event
//! loop, which libtest's worker threads do not have, so this is a `harness = false` test: `main`
//! runs `NSApplication` on the main thread and the scenarios run on a worker thread.
//!
//! Like an `#[ignore]`d test it does nothing unless `--ignored` (or `--include-ignored`) is
//! passed, as `.github/workflows/real-os.yml` does:
//!
//! ```sh
//! cargo test -p nugget-hotkey --test macos_it -- --ignored
//! ```
//!
//! # Permissions (TCC)
//!
//! - Posting key events (`CGEventPost`) needs the PostEvent / Accessibility permission for the
//!   process responsible for the test (on GitHub's runners, the runner's provisioner, which the
//!   image grants Accessibility).
//! - Reading and inserting through AX needs Accessibility.
//! - The modifier double tap itself (an NSEvent `flagsChanged` global monitor) and Carbon chords
//!   need no permission; the key-down monitor that cancels a tap used in a shortcut, and the
//!   Escape kill switch, need Accessibility.
//!
//! Every check that a missing permission makes impossible is skipped with the reason printed;
//! the test fails when nothing at all could run.

#[cfg(target_os = "macos")]
mod support;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if !args
        .iter()
        .any(|a| a == "--ignored" || a == "--include-ignored")
    {
        println!(
            "macos_it: skipped (presses real keys and opens TextEdit); run with -- --ignored \
             on a macOS desktop session"
        );
        return;
    }
    #[cfg(target_os = "macos")]
    mac::main();
    #[cfg(not(target_os = "macos"))]
    println!("macos_it: skipped (macOS only)");
}

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::mpsc;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    use nugget_a11y::Permission;
    use nugget_hotkey::{KillSwitch, Trigger, start, start_kill_switch};
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_core_graphics::{
        CGEvent, CGEventField, CGEventFlags, CGEventTapLocation, CGEventType,
        CGPreflightListenEventAccess, CGPreflightPostEventAccess,
    };

    use crate::support::{self, Mode, Pipeline};

    // kVK_* virtual key codes.
    const RIGHT_COMMAND: u16 = 54;
    const LEFT_COMMAND: u16 = 55;
    const ESCAPE: u16 = 53;
    const SPACE: u16 = 49;
    const F13: u16 = 105;

    // NX_DEVICE*KEYMASK bits (IOLLEvent.h), as `flagsChanged` carries them.
    const DEVICE_RCMD: u64 = 0x10;
    const DEVICE_LCMD: u64 = 0x08;

    fn post(ev: &CGEvent) {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(ev));
    }

    /// A Command key going down or up, as the hardware reports it: a `flagsChanged` event with
    /// the device-dependent bit of that key.
    fn command(keycode: u16, device_bit: u64, down: bool) {
        let ev = CGEvent::new_keyboard_event(None, keycode, down).expect("keyboard event");
        CGEvent::set_type(Some(&ev), CGEventType::FlagsChanged);
        CGEvent::set_integer_value_field(
            Some(&ev),
            CGEventField::KeyboardEventKeycode,
            i64::from(keycode),
        );
        let mut flags = CGEventFlags::MaskNonCoalesced.0;
        if down {
            flags |= CGEventFlags::MaskCommand.0 | device_bit;
        }
        CGEvent::set_flags(Some(&ev), CGEventFlags(flags));
        post(&ev);
        sleep(Duration::from_millis(25));
    }

    fn rcmd_tap() {
        command(RIGHT_COMMAND, DEVICE_RCMD, true);
        command(RIGHT_COMMAND, DEVICE_RCMD, false);
        sleep(Duration::from_millis(40));
    }

    fn lcmd_tap() {
        command(LEFT_COMMAND, DEVICE_LCMD, true);
        command(LEFT_COMMAND, DEVICE_LCMD, false);
        sleep(Duration::from_millis(40));
    }

    fn key(keycode: u16, flags: CGEventFlags) {
        for down in [true, false] {
            let ev = CGEvent::new_keyboard_event(None, keycode, down).expect("keyboard event");
            CGEvent::set_flags(Some(&ev), flags);
            post(&ev);
            sleep(Duration::from_millis(25));
        }
        sleep(Duration::from_millis(40));
    }

    fn channel_callback() -> (Box<dyn Fn() + Send + Sync>, mpsc::Receiver<Instant>) {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        (
            Box::new(move || {
                let _ = tx.lock().unwrap().send(Instant::now());
            }),
            rx,
        )
    }

    fn fired(rx: &mpsc::Receiver<Instant>) -> usize {
        let mut n = 0;
        while rx.recv_timeout(Duration::from_millis(300)).is_ok() {
            n += 1;
        }
        n
    }

    /// Start a listener from this worker thread. The installation is queued on the main thread,
    /// so give the run loop a moment before pressing keys.
    fn listen(trigger: Trigger) -> (Box<dyn nugget_hotkey::Listener>, mpsc::Receiver<Instant>) {
        let (cb, rx) = channel_callback();
        let l = start(trigger, cb).expect("start the listener");
        // The installation was queued on the main queue: once a later no-op has run there, the
        // monitors are in place.
        dispatch2::DispatchQueue::main().exec_sync(|| {});
        sleep(Duration::from_millis(100));
        (l, rx)
    }

    struct Perms {
        post: bool,
        ax: bool,
    }

    fn hotkeys(p: &Perms) -> Result<(), String> {
        if !p.post {
            return Err(skip(
                "hotkeys",
                "CGPreflightPostEventAccess() is false: this process may not post key events, \
                 so no key press can be synthesized",
            ));
        }
        let (listener, rx) = listen(Trigger::default());
        command(RIGHT_COMMAND, DEVICE_RCMD, true);
        command(RIGHT_COMMAND, DEVICE_RCMD, false);
        command(RIGHT_COMMAND, DEVICE_RCMD, true);
        // Measured from just before the second release is posted (`command` then waits 25 ms).
        let released = Instant::now();
        command(RIGHT_COMMAND, DEVICE_RCMD, false);
        let at = rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "the right-Command double tap did not fire within 2 s".to_string())?;
        println!(
            "DOUBLETAP-HOTKEY os=macos trigger=double_tap(RightCommand) key_up_to_fire_ms={:.2}",
            support::ms(at.saturating_duration_since(released))
        );
        check(fired(&rx) == 0, "one double tap fires once")?;

        lcmd_tap();
        lcmd_tap();
        check(
            fired(&rx) == 0,
            "left Command must not fire a right-Command listener",
        )?;

        rcmd_tap();
        sleep(Duration::from_millis(700));
        rcmd_tap();
        check(fired(&rx) == 0, "slow pair")?;

        rcmd_tap();
        rcmd_tap();
        rcmd_tap();
        check(fired(&rx) == 1, "triple tap fires once")?;

        if p.ax {
            // Right Command used in a shortcut (Cmd+F13 twice): the key-down monitor, which needs
            // Accessibility, cancels the tap.
            for _ in 0..2 {
                command(RIGHT_COMMAND, DEVICE_RCMD, true);
                key(F13, CGEventFlags(CGEventFlags::MaskCommand.0 | DEVICE_RCMD));
                command(RIGHT_COMMAND, DEVICE_RCMD, false);
            }
            check(fired(&rx) == 0, "right Command used in a shortcut")?;

            let (kcb, krx) = channel_callback();
            let kill = start_kill_switch(KillSwitch::default(), kcb).map_err(|e| e.to_string())?;
            sleep(Duration::from_millis(400));
            for _ in 0..3 {
                key(ESCAPE, CGEventFlags(0));
            }
            check(fired(&krx) == 1, "Escape x3 kill switch")?;
            check(fired(&rx) == 0, "Escape must not fire the double tap")?;
            drop(kill);
        } else {
            skip(
                "shortcut cancel and Escape kill switch",
                "Accessibility is not granted, so global key-down events are not delivered",
            );
        }

        // Chord through Carbon RegisterEventHotKey.
        let (chord, crx) = listen(Trigger::Chord("Ctrl+Alt+Space".into()));
        let pressed = Instant::now();
        key(
            SPACE,
            CGEventFlags(CGEventFlags::MaskControl.0 | CGEventFlags::MaskAlternate.0),
        );
        let at = crx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "Ctrl+Option+Space did not fire within 2 s".to_string())?;
        println!(
            "DOUBLETAP-HOTKEY os=macos trigger=chord(Ctrl+Alt+Space) press_to_fire_ms={:.2}",
            support::ms(at.saturating_duration_since(pressed))
        );
        check(fired(&crx) == 0, "one chord fires once")?;
        key(SPACE, CGEventFlags::MaskControl);
        check(fired(&crx) == 0, "Ctrl+Space is not the chord")?;

        drop(listener);
        drop(chord);
        sleep(Duration::from_millis(400));
        rcmd_tap();
        rcmd_tap();
        key(
            SPACE,
            CGEventFlags(CGEventFlags::MaskControl.0 | CGEventFlags::MaskAlternate.0),
        );
        check(fired(&rx) + fired(&crx) == 0, "fired after drop")?;
        println!("macos_it: hotkeys ok");
        Ok(())
    }

    fn is_textedit(app: &nugget_core::ipc::AppInfo) -> bool {
        app.id.as_deref() == Some("com.apple.TextEdit") || app.name == "TextEdit"
    }

    struct TextEdit {
        file: std::path::PathBuf,
    }

    impl Drop for TextEdit {
        fn drop(&mut self) {
            let _ = std::process::Command::new("/usr/bin/pkill")
                .args(["-9", "-x", "TextEdit"])
                .status();
            let _ = std::fs::remove_file(&self.file);
        }
    }

    fn insert(p: &Perms) -> Result<(), String> {
        if !p.ax {
            return Err(skip(
                "TextEdit insert",
                "Accessibility is not granted (AXIsProcessTrusted() is false), so the focused \
                 field can be neither read nor written",
            ));
        }
        let file =
            std::env::temp_dir().join(format!("nugget-doubletap-{}.txt", std::process::id()));
        std::fs::write(&file, b"").map_err(|e| e.to_string())?;
        let _guard = TextEdit { file: file.clone() };
        let status = std::process::Command::new("/usr/bin/open")
            .args(["-a", "TextEdit"])
            .arg(&file)
            .status()
            .map_err(|e| e.to_string())?;
        check(status.success(), "open -a TextEdit")?;
        let a11y = nugget_a11y::system();
        let focused =
            match support::wait_for_field(a11y.as_ref(), Duration::from_secs(8), is_textedit) {
                Ok(f) => f,
                Err(first) => {
                    // A process started from the runner may not be activated: bring TextEdit
                    // forward the way computer use does, and look again.
                    println!("macos_it: TextEdit is not in front yet ({first}); activating it");
                    nugget_a11y::system_computer()
                        .open_app("TextEdit")
                        .map_err(|e| format!("activate TextEdit: {e}"))?;
                    support::wait_for_field(a11y.as_ref(), Duration::from_secs(10), is_textedit)?
                }
            };
        println!(
            "TextEdit field: app={:?} id={:?} role={} value={:?} selection={:?}",
            focused.app.name,
            focused.app.id,
            focused.field.role,
            focused.field.value,
            focused.field.selection
        );
        check(!focused.field.secure, "TextEdit's text view is not secure")?;
        // The backend the shell's `doubletap::flow` builds on macOS.
        let ax = nugget_a11y::Backend::new(
            nugget_a11y::AxPlatform::new(),
            nugget_a11y::A11yConfig::default(),
        );
        support::read_breakdown(&ax, "macos");
        let front = || {
            ax.frontmost_app()
                .map(|f| format!("{:?}", f.map(|(a, _)| (a.name, a.window_title))))
                .unwrap_or_else(|e| format!("error: {e}"))
        };
        let result = (|| -> Result<(), String> {
            for (mode, chunks) in [
                (Mode::Shell, support::SHELL_CHUNKS),
                (Mode::Streamed, support::STREAMED_CHUNKS),
            ] {
                let pipeline = Pipeline::new(chunks);
                let report = if p.post {
                    let t = Instant::now();
                    let (listener, rx) = listen(Trigger::default());
                    println!(
                        "macos_it: listener installed in {:.0} ms; frontmost {}",
                        support::ms(t.elapsed()),
                        front()
                    );
                    // The draft starts on its own thread at the fire, and the listener stays up
                    // meanwhile, as in the shell. A missed double tap is retried and reported.
                    let report = support::trigger_and_draft(&ax, &pipeline, mode, rx, |attempt| {
                        if attempt > 1 {
                            println!("macos_it: frontmost before attempt {attempt}: {}", front());
                            sleep(Duration::from_millis(500));
                        }
                        rcmd_tap();
                        rcmd_tap();
                    });
                    drop(listener);
                    report
                } else {
                    println!(
                        "macos_it: key events cannot be posted; the draft starts as `doubletap_fire` \
                         does (no key press)"
                    );
                    support::draft_and_insert(&ax, &pipeline, Instant::now(), mode)
                }
                .map_err(|e| format!("{} mode: {e}", mode.as_str()))?;
                report.publish("macos");
                check(is_textedit(&report.app), "the draft was read from TextEdit")?;
            }
            Ok(())
        })();
        if let Err(e) = &result {
            println!("macos_it: insert failed ({e}); frontmost {}", front());
            screenshot("insert-failure");
        } else {
            println!("macos_it: TextEdit insert ok");
        }
        result
    }

    /// A screenshot into `$NUGGET_DOUBLETAP_ARTIFACTS`, taken while the failure is on screen.
    fn screenshot(name: &str) {
        if let Ok(dir) = std::env::var("NUGGET_DOUBLETAP_ARTIFACTS") {
            let path = std::path::Path::new(&dir).join(format!("{name}.png"));
            let _ = std::process::Command::new("/usr/sbin/screencapture")
                .arg("-x")
                .arg(&path)
                .status();
            println!("macos_it: screenshot {}", path.display());
        }
    }

    fn check(ok: bool, what: &str) -> Result<(), String> {
        if ok {
            Ok(())
        } else {
            Err(format!("check failed: {what}"))
        }
    }

    const SKIPPED: &str = "SKIPPED: ";

    fn skip(what: &str, why: &str) -> String {
        let msg = format!("{SKIPPED}{what}: {why}");
        println!("macos_it: {msg}");
        msg
    }

    fn run() -> i32 {
        let ax = nugget_a11y::system().permission() == Permission::Granted;
        let p = Perms {
            post: CGPreflightPostEventAccess(),
            ax,
        };
        println!(
            "macos_it: permissions: AXIsProcessTrusted={} CGPreflightPostEventAccess={} \
             CGPreflightListenEventAccess={}",
            p.ax,
            p.post,
            CGPreflightListenEventAccess()
        );
        let mut ran = 0;
        let mut failed = 0;
        for (name, result) in [("hotkeys", hotkeys(&p)), ("insert", insert(&p))] {
            match result {
                Ok(()) => {
                    ran += 1;
                    println!("test {name} ... ok");
                }
                Err(e) if e.starts_with(SKIPPED) => println!("test {name} ... skipped: {e}"),
                Err(e) => {
                    failed += 1;
                    println!("test {name} ... FAILED: {e}");
                }
            }
        }
        if failed > 0 {
            1
        } else if ran == 0 {
            println!("macos_it: FAILED: every scenario was skipped (see the reasons above)");
            1
        } else {
            0
        }
    }

    pub fn main() {
        let mtm = MainThreadMarker::new().expect("main() runs on the main thread");
        let app = NSApplication::sharedApplication(mtm);
        // No Dock icon and no activation: TextEdit must keep the focus.
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        std::thread::spawn(|| {
            sleep(Duration::from_secs(300));
            println!("macos_it: FAILED: timed out after 300 s");
            std::process::exit(2);
        });
        std::thread::spawn(|| {
            // Let the event loop start before the first listener is queued onto it.
            sleep(Duration::from_millis(300));
            let code = match std::panic::catch_unwind(run) {
                Ok(code) => code,
                Err(_) => {
                    println!("macos_it: FAILED: panicked");
                    1
                }
            };
            std::process::exit(code);
        });
        app.run();
    }
}
