//! Shared driver for the native-app integration tests (`windows_uia.rs`, `macos_ax.rs`): the
//! fixture app from `tests/fixtures/native/<os>/` (path in `NUGGET_A11Y_FIXTURE`), its state
//! file, and the scenarios, run through the public API (`system_with`, `system_computer_with`,
//! `system_clipboard`, `Backend::snapshot`).
//!
//! The fixture writes its own view of every control to a JSON state file, so each effect is
//! checked twice: through the accessibility API and by the app itself.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nugget_a11y::clipboard::{ClipboardFormat, ClipboardSnapshot};
use nugget_a11y::computer::KeyChord;
use nugget_a11y::role::Role;
use nugget_a11y::{
    A11yConfig, Accessibility, Backend, ComputerUse, DenyList, ElementId, Focused, InsertMethod,
    InsertStrategy, KeyInput, Platform, TreeLimits, UiAction, UiActionKind, UiNode, UiTree,
    WindowInfo, WindowSel,
};
use serde_json::Value;

pub const TITLE: &str = "Nugget A11y Fixture";
pub const APP: &str = "NuggetA11yFixture";
pub const ENV: &str = "NUGGET_A11Y_FIXTURE";

/// The running fixture app; killed on drop.
pub struct Fixture {
    state: PathBuf,
    child: Option<Child>,
}

impl Fixture {
    /// Launch the fixture named by `NUGGET_A11Y_FIXTURE` and wait for its first state.
    pub fn launch() -> Fixture {
        let path = std::env::var_os(ENV).map(PathBuf::from).unwrap_or_else(|| {
            panic!(
                "{ENV} is not set: build the fixture with tests/fixtures/native/<os>/build.* and \
                 point {ENV} at it (the real-os.yml a11y job does this)"
            )
        });
        assert!(path.exists(), "{ENV}={} does not exist", path.display());
        let state =
            std::env::temp_dir().join(format!("nugget-a11y-fixture-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&state);
        let child = if path.extension().is_some_and(|e| e == "app") {
            // An app bundle: through LaunchServices, which activates it like a user launch.
            let status = Command::new("/usr/bin/open")
                .arg("-n")
                .arg(&path)
                .arg("--args")
                .arg(&state)
                .status()
                .expect("run /usr/bin/open");
            assert!(status.success(), "open {} failed: {status}", path.display());
            None
        } else {
            let mut cmd = Command::new(&path);
            cmd.arg(&state)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit());
            Some(cmd.spawn().expect("start the fixture app"))
        };
        let fx = Fixture { state, child };
        fx.wait("the fixture to start", Duration::from_secs(60), |_| true);
        fx
    }

    /// The fixture's latest state, if it wrote one.
    pub fn state(&self) -> Option<Value> {
        let s = std::fs::read_to_string(&self.state).ok()?;
        serde_json::from_str(&s).ok()
    }

    /// Wait until the fixture's state satisfies `pred`; panics with the last state otherwise.
    pub fn wait(&self, what: &str, timeout: Duration, pred: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            let s = self.state();
            if let Some(s) = &s
                && pred(s)
            {
                return s.clone();
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for {what}; fixture state: {s:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait until `key` in the fixture's state equals `want`.
    pub fn expect(&self, key: &str, want: impl Into<Value>) -> Value {
        let want = want.into();
        self.wait(
            &format!("fixture {key} == {want}"),
            Duration::from_secs(5),
            |s| s.get(key) == Some(&want),
        )
    }

    pub fn pid(&self) -> Option<u32> {
        self.state()?
            .get("pid")?
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let pid = self.pid();
        match self.child.as_mut() {
            Some(c) => {
                let _ = c.kill();
                let _ = c.wait();
            }
            None => {
                if let Some(pid) = pid {
                    let _ = Command::new("kill").arg(pid.to_string()).status();
                }
            }
        }
        let _ = std::fs::remove_file(&self.state);
    }
}

pub fn config() -> A11yConfig {
    A11yConfig {
        deny: DenyList::defaults(),
        ..Default::default()
    }
}

fn is_fixture_window(title: Option<&str>) -> bool {
    title.is_some_and(|t| t.starts_with(TITLE))
}

/// Poll `focused()` until focus is in the fixture's window on a field labelled `label`. Nudges
/// the app to the front with `open_app` when it does not get there on its own.
pub fn wait_focus(
    a: &dyn Accessibility,
    cu: &dyn ComputerUse,
    fx: &Fixture,
    label: &str,
) -> Focused {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(40);
    let mut nudged = false;
    loop {
        let got = a.focused();
        if let Ok(Some(f)) = &got
            && is_fixture_window(f.app.window_title.as_deref())
            && f.field.label.as_deref() == Some(label)
        {
            eprintln!("focus reached {label:?} after {:?}", start.elapsed());
            return f.clone();
        }
        if !nudged && start.elapsed() > Duration::from_secs(8) {
            nudged = true;
            eprintln!(
                "focus not in the fixture yet ({got:?}); fixture state {:?}; open_app: {:?}",
                fx.state(),
                cu.open_app(APP)
            );
        }
        assert!(
            Instant::now() < deadline,
            "focus never reached {label:?} in the fixture: last focused() = {got:?}; \
             fixture state = {:?}",
            fx.state()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A node's accessible label: its name, else its description (AppKit puts
/// `accessibilityLabel` in `AXDescription`).
pub fn label(n: &UiNode) -> Option<&str> {
    n.name.as_deref().or(n.description.as_deref())
}

pub fn dump(t: &UiTree) -> String {
    serde_json::to_string_pretty(&t.root).unwrap_or_default()
}

pub fn find<'a>(t: &'a UiTree, roles: &[Role], name: &str) -> &'a UiNode {
    t.root
        .iter()
        .find(|n| roles.contains(&n.role) && label(n) == Some(name))
        .unwrap_or_else(|| panic!("no {roles:?} labelled {name:?} in\n{}", dump(t)))
}

/// A node whose name or value is `text` (static text shows its text either way).
pub fn has_text(t: &UiTree, text: &str) -> bool {
    t.root
        .iter()
        .any(|n| n.name.as_deref() == Some(text) || n.value.as_deref() == Some(text))
}

/// A list item or row that is, or contains, the text `text`.
pub fn find_item<'a>(t: &'a UiTree, text: &str) -> &'a UiNode {
    t.root
        .iter()
        .find(|n| {
            matches!(n.role, Role::ListItem | Role::Row)
                && n.iter()
                    .any(|c| c.name.as_deref() == Some(text) || c.value.as_deref() == Some(text))
        })
        .unwrap_or_else(|| panic!("no list item {text:?} in\n{}", dump(t)))
}

fn fixture_window(cu: &dyn ComputerUse) -> WindowInfo {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let ws = cu.windows().expect("windows()");
        if let Some(w) = ws.iter().find(|w| is_fixture_window(w.title.as_deref())) {
            return w.clone();
        }
        assert!(
            Instant::now() < deadline,
            "fixture window not listed: {ws:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn snap(cu: &dyn ComputerUse, sel: &WindowSel) -> UiTree {
    cu.snapshot(sel, &TreeLimits::default())
        .expect("snapshot of the fixture window")
}

/// Per-OS details the scenarios need.
pub struct Os<P> {
    /// The platform, for `Backend::snapshot` (the focused element's surroundings).
    pub platform: fn() -> P,
    /// A clipboard to seed before the paste test; it must be restored exactly afterwards.
    pub seed_clipboard: ClipboardSnapshot,
    /// Keys that move the caret to the end of a single-line field (`end`, `cmd+right`).
    pub end_of_line: &'static str,
}

pub fn text_format(format: &str, data: Vec<u8>) -> ClipboardFormat {
    ClipboardFormat {
        format: format.to_string(),
        data,
    }
}

fn formats(s: &ClipboardSnapshot) -> Vec<ClipboardFormat> {
    let mut v: Vec<ClipboardFormat> = s.items.iter().flatten().cloned().collect();
    v.sort_by(|a, b| a.format.cmp(&b.format));
    v
}

/// Every scenario, in order, against one fixture instance.
pub fn run<P: Platform>(os: Os<P>) {
    let t0 = Instant::now();
    let fx = Fixture::launch();
    eprintln!("fixture started in {:?}: {:?}", t0.elapsed(), fx.state());
    let a = nugget_a11y::system_with(config());
    let cu: Arc<dyn ComputerUse> = nugget_a11y::system_computer_with(config());

    // --- The focused element, its text and selection ---
    let f = wait_focus(a.as_ref(), cu.as_ref(), &fx, "Message composer");
    let t = Instant::now();
    let f2 = a.focused().unwrap().expect("focused");
    eprintln!("focused() took {:?}: {f:?}", t.elapsed());
    assert_eq!(f2.field, f.field);
    assert!(f.app.name.contains(APP), "app name {:?}", f.app.name);
    assert_eq!(f.field.role, "text_area");
    assert_eq!(f.field.value, "Hello world");
    assert_eq!(
        f.field.selection,
        Some((6, 11)),
        "the fixture selects \"world\""
    );
    assert!(!f.field.secure);
    let caret = f.caret.expect("caret (selection) bounds");
    assert!(caret.height > 0.0, "{caret:?}");

    // --- The tree around the focused element ---
    let b = Backend::new((os.platform)(), config());
    let t = Instant::now();
    let (app, around) = b.snapshot().unwrap().expect("snapshot around the focus");
    eprintln!(
        "Backend::snapshot took {:?}: {} nodes, truncated={}",
        t.elapsed(),
        around.root.count(),
        around.truncated
    );
    assert!(app.name.contains(APP));
    let fnode = around.focused().expect("focused node in the snapshot");
    assert!(fnode.states.focused && fnode.states.editable, "{fnode:?}");
    assert_eq!(fnode.role, Role::TextArea);
    assert!(
        around
            .root
            .iter()
            .any(|n| n.role == Role::Button && n.name.as_deref() == Some("Apply")),
        "Apply button in {}",
        around.to_json()
    );
    if let Ok(dir) = std::env::var("NUGGET_A11Y_RECORD") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            Path::new(&dir).join("focus_snapshot.json"),
            around.to_json(),
        );
    }

    // --- insert(): accessibility, replacing the selection, verified by read-back ---
    let out = a.insert("there ✓").expect("insert");
    assert_eq!(out.method, InsertMethod::Accessibility);
    assert!(out.verified);
    fx.expect("composer", "Hello there ✓");
    let f = a.focused().unwrap().expect("focused after insert");
    assert_eq!(f.field.value, "Hello there ✓");
    assert_eq!(
        f.field.selection,
        Some((13, 13)),
        "caret after the insertion"
    );

    // --- insert(): the clipboard path restores every format ---
    let mut clip = nugget_a11y::system_clipboard().expect("clipboard");
    clip.restore(&os.seed_clipboard)
        .expect("seed the clipboard");
    let before = clip.snapshot().expect("clipboard snapshot");
    for f in formats(&os.seed_clipboard) {
        assert!(
            formats(&before).contains(&f),
            "seeded format {} did not read back: {before:?}",
            f.format
        );
    }
    let pa = nugget_a11y::system_with(A11yConfig {
        insert: InsertStrategy::ClipboardOnly,
        ..config()
    });
    let out = pa.insert(" pasted").expect("clipboard insert");
    assert_eq!(out.method, InsertMethod::Clipboard);
    assert!(out.verified, "paste verified by read-back");
    fx.expect("composer", "Hello there ✓ pasted");
    let after = clip.snapshot().expect("clipboard snapshot");
    assert_eq!(
        formats(&after),
        formats(&before),
        "every clipboard format restored"
    );

    // --- Computer use: windows, a window snapshot, stable ids ---
    let win = fixture_window(cu.as_ref());
    eprintln!("fixture window: {win:?}");
    assert!(win.focused, "the fixture window is focused: {win:?}");
    assert!(win.app.name.contains(APP), "{:?}", win.app);
    assert!(win.bounds.is_some());
    let sel = WindowSel::Id(win.id.clone());
    let t = Instant::now();
    let tree = snap(cu.as_ref(), &sel);
    eprintln!(
        "window snapshot took {:?}: {} nodes, truncated={}",
        t.elapsed(),
        tree.count(),
        tree.truncated
    );
    if let Ok(dir) = std::env::var("NUGGET_A11Y_RECORD") {
        let _ = std::fs::write(Path::new(&dir).join("window_snapshot.json"), dump(&tree));
    }
    let apply = find(&tree, &[Role::Button], "Apply").clone();
    assert!(apply.actions.contains(&UiActionKind::Press), "{apply:?}");
    let check = find(&tree, &[Role::CheckBox], "Notifications").clone();
    assert_eq!(check.states.checked, Some(false), "{check:?}");
    assert!(check.actions.contains(&UiActionKind::Press), "{check:?}");
    let name = find(&tree, &[Role::TextField], "Name").clone();
    assert!(name.actions.contains(&UiActionKind::SetValue), "{name:?}");
    let composer = find(
        &tree,
        &[Role::TextArea, Role::TextField],
        "Message composer",
    )
    .clone();
    assert_eq!(composer.value.as_deref(), Some("Hello there ✓ pasted"));
    assert!(composer.states.focused, "{composer:?}");
    assert_eq!(tree.focused.as_ref(), Some(&composer.id));
    for fruit in ["Apple", "Banana", "Cherry"] {
        find_item(&tree, fruit);
    }
    assert!(
        has_text(&tree, "Status: idle"),
        "status label in\n{}",
        dump(&tree)
    );
    let again = snap(cu.as_ref(), &sel);
    assert_eq!(
        find(&again, &[Role::Button], "Apply").id,
        apply.id,
        "ids are stable across snapshots"
    );

    // --- act: set a value and read it back ---
    let out = cu
        .act(
            &name.id,
            &UiAction::SetValue {
                value: "Ada".into(),
            },
        )
        .expect("set value");
    assert!(out.verified, "set value not verified: {}", out.detail);
    fx.expect("name", "Ada");
    let tree = snap(cu.as_ref(), &sel);
    assert_eq!(
        find(&tree, &[Role::TextField], "Name").value.as_deref(),
        Some("Ada")
    );

    // --- act: press the button; the status label and the window title change ---
    let out = cu.act(&apply.id, &UiAction::Press).expect("press");
    assert!(out.verified, "press not verified: {}", out.detail);
    fx.expect("status", "Status: applied 1");
    fx.expect("presses", 1);
    let tree = snap(cu.as_ref(), &sel);
    assert!(has_text(&tree, "Status: applied 1"), "{}", dump(&tree));
    let title = fixture_window(cu.as_ref()).title;
    assert_eq!(title.as_deref(), Some("Nugget A11y Fixture - applied 1"));

    // --- act: toggle the check box ---
    let out = cu.act(&check.id, &UiAction::Press).expect("toggle");
    assert!(out.verified, "toggle not verified: {}", out.detail);
    assert_eq!(
        out.after.as_ref().and_then(|n| n.states.checked),
        Some(true)
    );
    fx.expect("checked", true);

    // --- act: select a list item ---
    let cherry = find_item(&snap(cu.as_ref(), &sel), "Cherry").id.clone();
    let out = cu.act(&cherry, &UiAction::Select).expect("select");
    assert!(out.verified, "select not verified: {}", out.detail);
    fx.expect("fruit", "Cherry");

    // --- key input into the name field, after the existing text ---
    let end = KeyInput::Keys(KeyChord::parse_list(os.end_of_line).unwrap());
    let out = cu.key_input(Some(&name.id), &end).expect("end key");
    eprintln!("end key: verified={} {}", out.verified, out.detail);
    let out = cu
        .key_input(Some(&name.id), &KeyInput::Text(" Lovelace".into()))
        .expect("type");
    assert!(out.verified, "typing not verified: {}", out.detail);
    fx.expect("name", "Ada Lovelace");

    // --- focus back on the composer; focused() follows ---
    let composer_id: ElementId = composer.id.clone();
    let out = cu.act(&composer_id, &UiAction::Focus).expect("focus");
    assert!(out.verified, "focus not verified: {}", out.detail);
    let f = wait_focus(a.as_ref(), cu.as_ref(), &fx, "Message composer");
    assert_eq!(f.field.value, "Hello there ✓ pasted");
    fx.expect("focus", "composer");

    eprintln!("native a11y scenarios passed in {:?}", t0.elapsed());
}
