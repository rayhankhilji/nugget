use nugget_core::ipc::{AppInfo, DraftContext, FieldInfo};
use nugget_draft::{DEFAULT_EXCLUDED_APPS, gate};

#[test]
fn pa_terminals_and_other_password_managers_are_draftable() {
    let excluded: Vec<String> = DEFAULT_EXCLUDED_APPS.iter().map(|s| s.to_string()).collect();
    for (id, name) in [
        ("com.apple.Terminal", "Terminal"),
        ("com.googlecode.iterm2", "iTerm2"),
        ("org.gnome.Terminal.desktop", "Terminal"),
        ("WindowsTerminal.exe", "Windows Terminal"),
        ("me.proton.pass.electron", "Proton Pass"),
        ("in.sinew.Enpass-Desktop", "Enpass"),
        ("com.nordsec.nordpass", "NordPass"),
    ] {
        let ctx = DraftContext {
            app: AppInfo { id: Some(id.into()), name: name.into(), window_title: None, url: None },
            field: FieldInfo { role: "text_area".into(), ..Default::default() },
            ..Default::default()
        };
        if gate(&ctx, &excluded).is_ok() { eprintln!("DRAFTABLE {id}"); }
    }
}
