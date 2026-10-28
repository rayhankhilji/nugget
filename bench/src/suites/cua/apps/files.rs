//! File manager mock over a virtual tree: browse, preview, new folder, rename, move, move to
//! Trash, empty Trash.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{App, Emitted, unknown};
use crate::suites::cua::ui::{Action, Node, slug};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    File,
    Dir,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// Absolute path, `/Documents/notes.txt`.
    pub path: String,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Files {
    pub entries: Vec<Entry>,
    pub trash: Vec<Entry>,
    pub cwd: String,
    /// Name of the selected entry in `cwd`.
    pub selected: Option<String>,
    /// Path of the file being previewed.
    pub preview: Option<String>,
    pub name_input: String,
    pub dest_input: String,
}

fn dir(p: &str) -> Entry {
    Entry {
        path: p.into(),
        kind: Kind::Dir,
        content: String::new(),
    }
}

fn file(p: &str, content: &str) -> Entry {
    Entry {
        path: p.into(),
        kind: Kind::File,
        content: content.into(),
    }
}

fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn join(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn is_under(path: &str, dir: &str) -> bool {
    path == dir || path.starts_with(&format!("{dir}/"))
}

fn valid_name(n: &str) -> bool {
    let n = n.trim();
    !n.is_empty() && !n.contains('/') && n != "." && n != ".."
}

impl Files {
    pub fn fixture() -> Self {
        Self {
            entries: vec![
                dir("/Documents"),
                file("/Documents/draft.txt", "Quarterly plan, draft 3."),
                file(
                    "/Documents/notes.txt",
                    "Meeting notes: ship the beta on Friday.",
                ),
                file("/Documents/report-2023.pdf", "Annual report 2023"),
                file("/Documents/report-2024.pdf", "Annual report 2024"),
                dir("/Downloads"),
                file("/Downloads/old-backup.zip", "backup"),
                file("/Downloads/receipt-001.pdf", "Receipt 001: $12.00"),
                file("/Downloads/receipt-002.pdf", "Receipt 002: $48.50"),
                file("/Downloads/receipt-003.pdf", "Receipt 003: $7.25"),
                file("/Downloads/beach.jpg", "jpeg"),
                file("/Downloads/invoice.pdf", "Invoice"),
                dir("/Pictures"),
                dir("/Photos"),
                file("/Photos/family-2025.jpg", "jpeg"),
                file("/Photos/wedding.jpg", "jpeg"),
            ],
            trash: Vec::new(),
            cwd: "/".into(),
            selected: None,
            preview: None,
            name_input: String::new(),
            dest_input: String::new(),
        }
    }

    fn get(&self, path: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.path == path)
    }

    fn is_dir(&self, path: &str) -> bool {
        path == "/" || self.get(path).is_some_and(|e| e.kind == Kind::Dir)
    }

    fn children(&self, dir: &str) -> Vec<&Entry> {
        let mut v: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| parent(&e.path) == dir && e.path != dir)
            .collect();
        v.sort_by(|a, b| (a.kind == Kind::File, &a.path).cmp(&(b.kind == Kind::File, &b.path)));
        v
    }

    fn selected_path(&self) -> Result<String, String> {
        let name = self.selected.as_deref().ok_or("Select an item first")?;
        let p = join(&self.cwd, name);
        if self.get(&p).is_none() {
            return Err("The selected item no longer exists".into());
        }
        Ok(p)
    }

    /// Re-root `from` (and everything under it) at `to`.
    fn relocate(&mut self, from: &str, to: &str) {
        for e in &mut self.entries {
            if is_under(&e.path, from) {
                e.path = format!("{to}{}", &e.path[from.len()..]);
            }
        }
    }

    fn resolve_dir(&self, input: &str) -> Result<String, String> {
        let t = input.trim().trim_end_matches('/');
        if t.is_empty() && !input.trim().starts_with('/') {
            return Err("Enter a destination folder".into());
        }
        let p = if input.trim().starts_with('/') {
            if t.is_empty() {
                "/".to_owned()
            } else {
                t.to_owned()
            }
        } else {
            join(&self.cwd, t)
        };
        if self.is_dir(&p) {
            Ok(p)
        } else {
            Err(format!("No folder {p}"))
        }
    }
}

impl App for Files {
    fn id(&self) -> &'static str {
        "files"
    }

    fn view(&self) -> Node {
        let items = self.children(&self.cwd).into_iter().map(|e| {
            let name = base(&e.path);
            let label = match e.kind {
                Kind::Dir => format!("Folder {name}"),
                Kind::File => format!("File {name}"),
            };
            Node::item(format!("files.row.{}", slug(name)), name)
                .child(Node::button(format!("files.item.{}", slug(name)), label))
        });
        let mut w = Node::window("files", format!("Files — {}", self.cwd))
            .child(Node::button("files.up", "Up one folder").enabled(self.cwd != "/"))
            .child(Node::list("files.list", "Contents").children(items))
            .child(Node::text(
                "files.selected",
                format!(
                    "Selected: {}",
                    self.selected.as_deref().unwrap_or("nothing")
                ),
            ))
            .child(Node::button("files.open", "Open").enabled(self.selected.is_some()))
            .child(
                Node::button("files.delete", "Move to Trash")
                    .enabled(self.selected.is_some())
                    .emits("delete"),
            )
            .child(Node::textbox("files.name", "Name", &self.name_input))
            .child(Node::button("files.mkdir", "New folder"))
            .child(Node::button("files.rename", "Rename").enabled(self.selected.is_some()))
            .child(Node::textbox(
                "files.dest",
                "Destination folder",
                &self.dest_input,
            ))
            .child(Node::button("files.move", "Move").enabled(self.selected.is_some()));
        if let Some(p) = &self.preview
            && let Some(e) = self.get(p)
        {
            w = w.child(
                Node::group("files.preview", format!("Preview: {}", base(p)))
                    .child(Node::text("files.preview.content", &e.content)),
            );
        }
        w.child(
            Node::group("files.trash", format!("Trash ({})", self.trash.len()))
                .children(
                    self.trash
                        .iter()
                        .enumerate()
                        .map(|(i, e)| Node::text(format!("files.trash.{i}"), &e.path)),
                )
                .child(
                    Node::button("files.empty_trash", "Empty Trash")
                        .enabled(!self.trash.is_empty())
                        .emits("empty_trash"),
                ),
        )
    }

    fn handle(&mut self, action: &Action) -> Result<Vec<Emitted>, String> {
        match action {
            Action::Click { target } => match target.as_str() {
                "files.up" => {
                    self.cwd = parent(&self.cwd).to_owned();
                    self.selected = None;
                    self.preview = None;
                }
                "files.open" => {
                    let p = self.selected_path()?;
                    if self.is_dir(&p) {
                        self.cwd = p;
                        self.selected = None;
                        self.preview = None;
                    } else {
                        self.preview = Some(p);
                    }
                }
                "files.mkdir" => {
                    let name = self.name_input.trim().to_owned();
                    if !valid_name(&name) {
                        return Err("Enter a folder name".into());
                    }
                    let p = join(&self.cwd, &name);
                    if self.get(&p).is_some() {
                        return Err(format!("{name} already exists"));
                    }
                    self.entries.push(dir(&p));
                    self.name_input.clear();
                    return Ok(vec![Emitted::new("mkdir", json!({ "path": p }))]);
                }
                "files.rename" => {
                    let from = self.selected_path()?;
                    let name = self.name_input.trim().to_owned();
                    if !valid_name(&name) {
                        return Err("Enter the new name".into());
                    }
                    let to = join(&self.cwd, &name);
                    if self.get(&to).is_some() {
                        return Err(format!("{name} already exists"));
                    }
                    self.relocate(&from, &to);
                    self.selected = Some(name);
                    self.name_input.clear();
                    if self.preview.as_deref() == Some(from.as_str()) {
                        self.preview = Some(to.clone());
                    }
                    return Ok(vec![Emitted::new(
                        "rename",
                        json!({"from": from, "to": to}),
                    )]);
                }
                "files.move" => {
                    let from = self.selected_path()?;
                    let dest = self.resolve_dir(&self.dest_input.clone())?;
                    if is_under(&dest, &from) {
                        return Err("Cannot move a folder into itself".into());
                    }
                    let to = join(&dest, base(&from));
                    if to == from {
                        return Err("Already there".into());
                    }
                    if self.get(&to).is_some() {
                        return Err(format!("{to} already exists"));
                    }
                    self.relocate(&from, &to);
                    self.selected = None;
                    self.preview = None;
                    return Ok(vec![Emitted::new("move", json!({"from": from, "to": to}))]);
                }
                "files.delete" => {
                    let p = self.selected_path()?;
                    let (gone, keep): (Vec<Entry>, Vec<Entry>) = std::mem::take(&mut self.entries)
                        .into_iter()
                        .partition(|e| is_under(&e.path, &p));
                    self.entries = keep;
                    let paths: Vec<&str> = gone.iter().map(|e| e.path.as_str()).collect();
                    let detail = json!({"path": p, "paths": paths});
                    self.trash.extend(gone);
                    self.selected = None;
                    self.preview = None;
                    return Ok(vec![Emitted::new("delete", detail)]);
                }
                "files.empty_trash" => {
                    let paths: Vec<String> = self.trash.drain(..).map(|e| e.path).collect();
                    return Ok(vec![Emitted::new("empty_trash", json!({ "paths": paths }))]);
                }
                t => {
                    let Some(s) = t.strip_prefix("files.item.") else {
                        return unknown(action);
                    };
                    let name = self
                        .children(&self.cwd)
                        .into_iter()
                        .map(|e| base(&e.path).to_owned())
                        .find(|n| slug(n) == s)
                        .ok_or("no such item")?;
                    self.selected = Some(name);
                }
            },
            Action::Type { target, text } => match target.as_str() {
                "files.name" => self.name_input = text.clone(),
                "files.dest" => self.dest_input = text.clone(),
                _ => return unknown(action),
            },
            Action::Select { .. } => return unknown(action),
        }
        Ok(Vec::new())
    }

    fn state(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(t: &str) -> Action {
        Action::Click { target: t.into() }
    }
    fn ty(t: &str, s: &str) -> Action {
        Action::Type {
            target: t.into(),
            text: s.into(),
        }
    }

    #[test]
    fn browse_rename_move_delete() {
        let mut f = Files::fixture();
        f.handle(&click("files.item.documents")).unwrap();
        f.handle(&click("files.open")).unwrap();
        assert_eq!(f.cwd, "/Documents");
        f.handle(&click("files.item.draft_txt")).unwrap();
        f.handle(&ty("files.name", "final.txt")).unwrap();
        f.handle(&click("files.rename")).unwrap();
        assert!(f.get("/Documents/final.txt").is_some());
        f.handle(&ty("files.name", "Archive")).unwrap();
        f.handle(&click("files.mkdir")).unwrap();
        f.handle(&click("files.item.final_txt")).unwrap();
        f.handle(&ty("files.dest", "Archive")).unwrap();
        f.handle(&click("files.move")).unwrap();
        assert!(f.get("/Documents/Archive/final.txt").is_some());
        f.handle(&click("files.item.archive")).unwrap();
        let ev = f.handle(&click("files.delete")).unwrap();
        assert_eq!(ev[0].kind, "delete");
        assert_eq!(f.trash.len(), 2, "folder and its file");
        assert!(f.get("/Documents/Archive/final.txt").is_none());
        let ev = f.handle(&click("files.empty_trash")).unwrap();
        assert_eq!(ev[0].kind, "empty_trash");
        assert!(f.trash.is_empty());
        f.handle(&click("files.up")).unwrap();
        assert_eq!(f.cwd, "/");
    }

    #[test]
    fn move_rejects_bad_destinations() {
        let mut f = Files::fixture();
        f.handle(&click("files.item.photos")).unwrap();
        f.handle(&ty("files.dest", "/Photos")).unwrap();
        assert!(f.handle(&click("files.move")).is_err());
        f.handle(&ty("files.dest", "/Nowhere")).unwrap();
        assert!(f.handle(&click("files.move")).is_err());
        f.handle(&ty("files.dest", "/Documents")).unwrap();
        f.handle(&click("files.move")).unwrap();
        assert!(f.get("/Documents/Photos/wedding.jpg").is_some());
    }

    #[test]
    fn preview_shows_content() {
        let mut f = Files::fixture();
        f.cwd = "/Documents".into();
        f.handle(&click("files.item.notes_txt")).unwrap();
        f.handle(&click("files.open")).unwrap();
        let v = f.view();
        assert!(
            v.find("files.preview.content")
                .is_some_and(|n| n.name.contains("beta"))
        );
    }
}
