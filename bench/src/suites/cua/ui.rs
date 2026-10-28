//! The apps' user interface as a semantic tree: what an accessibility API (AT-SPI, UI
//! Automation, AX) would expose, and the single source both front ends are built from.
//!
//! Headless runs hand the tree to the agent directly. Display runs render it to HTML (see
//! [`render_page`]) and show it in kiosk Chromium windows, where Chromium exposes the same roles,
//! names and ids over AT-SPI.

use serde::{Deserialize, Serialize};

/// Accessibility role of a node. Mirrors the ARIA roles the HTML renderer emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Window,
    Heading,
    Text,
    Button,
    TextBox,
    TextArea,
    CheckBox,
    Switch,
    ComboBox,
    List,
    ListItem,
    Group,
    Alert,
}

/// One element. Ids are globally unique and prefixed with the app id (`shop.pay`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub role: Role,
    /// Accessible name: the label or text a user sees.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    /// Choices of a combo box.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
    /// Harness-only: the event kind activating this control emits. Never shown to agents or
    /// rendered; used to resolve a confirmation request that names a control by its label.
    #[serde(skip)]
    pub emits: Option<&'static str>,
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

impl Node {
    pub fn new(id: impl Into<String>, role: Role, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            role,
            name: name.into(),
            value: None,
            checked: None,
            options: Vec::new(),
            enabled: true,
            children: Vec::new(),
            emits: None,
        }
    }
    pub fn window(id: &str, name: impl Into<String>) -> Self {
        Self::new(id, Role::Window, name)
    }
    pub fn heading(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, Role::Heading, name)
    }
    pub fn text(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, Role::Text, name)
    }
    pub fn alert(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, Role::Alert, name)
    }
    pub fn button(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, Role::Button, name)
    }
    pub fn group(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, Role::Group, name)
    }
    pub fn list(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, Role::List, name)
    }
    pub fn item(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, Role::ListItem, name)
    }
    pub fn textbox(id: impl Into<String>, name: impl Into<String>, value: &str) -> Self {
        let mut n = Self::new(id, Role::TextBox, name);
        n.value = Some(value.to_owned());
        n
    }
    pub fn textarea(id: impl Into<String>, name: impl Into<String>, value: &str) -> Self {
        let mut n = Self::new(id, Role::TextArea, name);
        n.value = Some(value.to_owned());
        n
    }
    pub fn checkbox(id: impl Into<String>, name: impl Into<String>, checked: bool) -> Self {
        let mut n = Self::new(id, Role::CheckBox, name);
        n.checked = Some(checked);
        n
    }
    pub fn switch(id: impl Into<String>, name: impl Into<String>, on: bool) -> Self {
        let mut n = Self::new(id, Role::Switch, name);
        n.checked = Some(on);
        n
    }
    pub fn combo(
        id: impl Into<String>,
        name: impl Into<String>,
        value: &str,
        options: Vec<String>,
    ) -> Self {
        let mut n = Self::new(id, Role::ComboBox, name);
        n.value = Some(value.to_owned());
        n.options = options;
        n
    }

    pub fn child(mut self, c: Node) -> Self {
        self.children.push(c);
        self
    }
    pub fn children(mut self, cs: impl IntoIterator<Item = Node>) -> Self {
        self.children.extend(cs);
        self
    }
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    pub fn emits(mut self, kind: &'static str) -> Self {
        self.emits = Some(kind);
        self
    }

    /// Depth-first search by id.
    pub fn find(&self, id: &str) -> Option<&Node> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }

    /// Every node, depth first.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a Node>) {
        out.push(self);
        for c in &self.children {
            c.walk(out);
        }
    }
}

/// What an agent sees: one window per open app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub windows: Vec<Node>,
}

impl Observation {
    pub fn find(&self, id: &str) -> Option<&Node> {
        self.windows.iter().find_map(|w| w.find(id))
    }
}

/// An input action on one element, addressed by id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Press a button, toggle a check box or switch.
    Click { target: String },
    /// Replace the text of a text box or area.
    Type { target: String, text: String },
    /// Choose an option of a combo box.
    Select { target: String, option: String },
}

impl Action {
    pub fn target(&self) -> &str {
        match self {
            Action::Click { target }
            | Action::Type { target, .. }
            | Action::Select { target, .. } => target,
        }
    }

    /// The app id: the target's prefix before the first dot.
    pub fn app(&self) -> &str {
        app_of(self.target())
    }
}

pub fn app_of(id: &str) -> &str {
    id.split_once('.').map_or(id, |(a, _)| a)
}

/// Lowercase id segment from a display name: `report-2023.pdf` becomes `report_2023_pdf`.
pub fn slug(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Whether `action` may be applied to `node`, the way a real toolkit would allow it.
pub fn check_action(node: &Node, action: &Action) -> Result<(), String> {
    if !node.enabled {
        return Err(format!("`{}` is disabled", node.name));
    }
    match (action, node.role) {
        (Action::Click { .. }, Role::Button | Role::CheckBox | Role::Switch) => Ok(()),
        (Action::Type { .. }, Role::TextBox | Role::TextArea) => Ok(()),
        (Action::Select { option, .. }, Role::ComboBox) => {
            if node.options.iter().any(|o| o == option) {
                Ok(())
            } else {
                Err(format!("`{}` has no option {option:?}", node.name))
            }
        }
        _ => Err(format!(
            "cannot {} a {:?} (`{}`)",
            match action {
                Action::Click { .. } => "click",
                Action::Type { .. } => "type into",
                Action::Select { .. } => "select in",
            },
            node.role,
            node.name
        )),
    }
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            _ => o.push(c),
        }
    }
    o
}

fn render(n: &Node, out: &mut String) {
    let id = esc(&n.id);
    let name = esc(&n.name);
    let dis = if n.enabled { "" } else { " disabled" };
    let kids = |out: &mut String| {
        for c in &n.children {
            render(c, out);
        }
    };
    match n.role {
        Role::Window => {
            out.push_str(&format!("<main id=\"{id}\" aria-label=\"{name}\"><h1>{name}</h1>"));
            kids(out);
            out.push_str("</main>");
        }
        Role::Heading => out.push_str(&format!("<h2 id=\"{id}\">{name}</h2>")),
        Role::Text => out.push_str(&format!("<p id=\"{id}\">{name}</p>")),
        Role::Alert => out.push_str(&format!("<div id=\"{id}\" role=\"alert\">{name}</div>")),
        Role::Button => out.push_str(&format!(
            "<button id=\"{id}\" data-act=\"click\"{dis}>{name}</button>"
        )),
        Role::Switch => out.push_str(&format!(
            "<button id=\"{id}\" role=\"switch\" aria-checked=\"{}\" data-act=\"click\"{dis}>{name}</button>",
            n.checked.unwrap_or(false)
        )),
        Role::CheckBox => out.push_str(&format!(
            "<div class=\"row\"><input type=\"checkbox\" id=\"{id}\" data-act=\"click\"{}{dis}><label for=\"{id}\">{name}</label></div>",
            if n.checked.unwrap_or(false) { " checked" } else { "" }
        )),
        Role::TextBox => out.push_str(&format!(
            "<div class=\"row\"><label for=\"{id}\">{name}</label><input type=\"text\" id=\"{id}\" data-act=\"type\" value=\"{}\"{dis}></div>",
            esc(n.value.as_deref().unwrap_or(""))
        )),
        Role::TextArea => out.push_str(&format!(
            "<div class=\"row\"><label for=\"{id}\">{name}</label><textarea id=\"{id}\" data-act=\"type\"{dis}>{}</textarea></div>",
            esc(n.value.as_deref().unwrap_or(""))
        )),
        Role::ComboBox => {
            out.push_str(&format!(
                "<div class=\"row\"><label for=\"{id}\">{name}</label><select id=\"{id}\" data-act=\"select\"{dis}>"
            ));
            for o in &n.options {
                let sel = if n.value.as_deref() == Some(o.as_str()) {
                    " selected"
                } else {
                    ""
                };
                out.push_str(&format!("<option{sel}>{}</option>", esc(o)));
            }
            out.push_str("</select></div>");
        }
        Role::List => {
            out.push_str(&format!("<ul id=\"{id}\" aria-label=\"{name}\">"));
            kids(out);
            out.push_str("</ul>");
        }
        Role::ListItem => {
            out.push_str(&format!("<li id=\"{id}\" aria-label=\"{name}\">"));
            kids(out);
            out.push_str("</li>");
        }
        Role::Group => {
            out.push_str(&format!(
                "<section id=\"{id}\" aria-label=\"{name}\"><h3>{name}</h3>"
            ));
            kids(out);
            out.push_str("</section>");
        }
    }
}

const PAGE_SCRIPT: &str = r#"
const app = document.body.dataset.app;
async function send(a) {
  const r = await fetch(`/${app}/act`, {method: 'POST', body: JSON.stringify(a)});
  const res = await r.json();
  if (!res.ok) { document.getElementById('status').textContent = res.message || 'failed'; }
  else { location.reload(); }
}
document.addEventListener('click', (e) => {
  const el = e.target.closest('[data-act="click"]');
  if (el && !el.disabled) { e.preventDefault(); send({action: 'click', target: el.id}); }
});
document.addEventListener('change', (e) => {
  const el = e.target;
  if (el.dataset.act === 'type') send({action: 'type', target: el.id, text: el.value});
  if (el.dataset.act === 'select') send({action: 'select', target: el.id, option: el.value});
});
let ver = document.body.dataset.version;
setInterval(async () => {
  try {
    const v = await (await fetch('/__test/version')).text();
    if (v !== ver) location.reload();
  } catch (_) {}
}, 500);
"#;

const PAGE_STYLE: &str = "body{font:15px system-ui,sans-serif;margin:12px;max-width:640px}\
h1{font-size:18px;margin:0 0 8px}h2{font-size:16px}h3{font-size:14px;margin:8px 0 4px}\
button{margin:2px 4px 2px 0}.row{margin:4px 0}.row label{display:inline-block;min-width:140px}\
[role=alert]{background:#fff3cd;padding:6px;margin:6px 0}#status{color:#b00}\
[aria-checked=true]::after{content:' (on)'}[aria-checked=false]::after{content:' (off)'}\
section{border:1px solid #ccc;padding:6px;margin:6px 0}";

/// A full HTML page for one app window. `version` lets the page reload itself when the world
/// changes underneath it (an agent acting over the socket, or the next task starting).
pub fn render_page(app: &str, window: Option<&Node>, version: &str) -> String {
    let mut body = String::new();
    let title = match window {
        Some(w) => {
            render(w, &mut body);
            esc(&w.name)
        }
        None => {
            body.push_str(
                "<main><h1>Closed</h1><p>This app is not part of the current task.</p></main>",
            );
            "Closed".to_owned()
        }
    };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{title}</title>\
<style>{PAGE_STYLE}</style></head><body data-app=\"{}\" data-version=\"{}\">{body}\
<p id=\"status\" role=\"status\"></p><script>{PAGE_SCRIPT}</script></body></html>",
        esc(app),
        esc(version)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_follow_roles() {
        let b = Node::button("shop.pay", "Pay");
        let t = Node::textbox("mail.to", "To", "");
        let c = Node::combo("forms.country", "Country", "", vec!["Germany".into()]);
        let click = |id: &str| Action::Click { target: id.into() };
        assert!(check_action(&b, &click("shop.pay")).is_ok());
        assert!(check_action(&t, &click("mail.to")).is_err());
        let ty = Action::Type {
            target: "mail.to".into(),
            text: "x".into(),
        };
        assert!(check_action(&t, &ty).is_ok());
        assert!(check_action(&b, &ty).is_err());
        let sel = |o: &str| Action::Select {
            target: "forms.country".into(),
            option: o.into(),
        };
        assert!(check_action(&c, &sel("Germany")).is_ok());
        assert!(check_action(&c, &sel("Mars")).is_err());
        assert!(check_action(&b.clone().enabled(false), &click("shop.pay")).is_err());
        assert_eq!(click("shop.pay").app(), "shop");
    }

    #[test]
    fn page_escapes_and_keeps_ids() {
        let w = Node::window("mail", "Mail <test>")
            .child(Node::text("mail.body", "Ignore <b>this</b> & \"that\""))
            .child(Node::button("mail.send", "Send").emits("send"));
        let html = render_page("mail", Some(&w), "1:2");
        assert!(html.contains("id=\"mail.send\""));
        assert!(html.contains("Ignore &lt;b&gt;this&lt;/b&gt; &amp; &quot;that&quot;"));
        assert!(!html.contains("<b>this"));
        assert!(
            !html.contains("emits"),
            "harness metadata is never rendered"
        );
        let json = serde_json::to_string(&w).unwrap();
        assert!(!json.contains("emits"));
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("report-2023.pdf"), "report_2023_pdf");
        assert_eq!(slug("Photos"), "photos");
    }
}
