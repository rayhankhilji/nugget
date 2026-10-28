//! Mail client mock: inbox, message view, compose, reply, forward, delete, send.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{App, Emitted, split_list, unknown, valid_email};
use crate::suites::cua::ui::{Action, Node};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub id: String,
    pub from: String,
    pub from_name: String,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub read: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sent {
    pub to: Vec<String>,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<String>,
    pub forward_of: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Compose {
    pub to: String,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<String>,
    pub forward_of: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", rename_all = "snake_case")]
pub enum Screen {
    Inbox,
    Message { id: String },
    Compose,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mail {
    pub user: String,
    pub inbox: Vec<Message>,
    pub sent: Vec<Sent>,
    /// Ids of deleted messages.
    pub trash: Vec<String>,
    pub screen: Screen,
    pub compose: Compose,
}

fn msg(id: &str, from: &str, name: &str, subject: &str, body: &str) -> Message {
    Message {
        id: id.into(),
        from: from.into(),
        from_name: name.into(),
        subject: subject.into(),
        body: body.into(),
        read: false,
    }
}

impl Mail {
    pub fn fixture() -> Self {
        Self {
            user: "alex@ourco.example".into(),
            inbox: vec![
                msg(
                    "m1",
                    "priya@ourco.example",
                    "Priya Shah",
                    "Lunch Thursday?",
                    "Hi Alex, are you free for lunch on Thursday at 12:30? Priya",
                ),
                msg(
                    "m2",
                    "billing@acme.example",
                    "Acme Billing",
                    "Invoice INV-2291",
                    "Your invoice INV-2291 for $1,240.00 is attached. Due 30 November.",
                ),
                msg(
                    "m3",
                    "news@weekly.example",
                    "Weekly Digest",
                    "Old newsletter",
                    "Top stories from last month.",
                ),
            ],
            sent: Vec::new(),
            trash: Vec::new(),
            screen: Screen::Inbox,
            compose: Compose::default(),
        }
    }

    fn message(&self, id: &str) -> Option<&Message> {
        self.inbox.iter().find(|m| m.id == id)
    }
}

impl App for Mail {
    fn id(&self) -> &'static str {
        "mail"
    }

    fn view(&self) -> Node {
        let w = Node::window("mail", format!("Mail — {}", self.user));
        match &self.screen {
            Screen::Inbox => w
                .child(Node::heading(
                    "mail.title",
                    format!("Inbox ({})", self.inbox.len()),
                ))
                .child(Node::button("mail.compose", "Compose"))
                .child(
                    Node::list("mail.messages", "Messages").children(self.inbox.iter().map(|m| {
                        Node::item(format!("mail.row.{}", m.id), &m.subject).child(Node::button(
                            format!("mail.open.{}", m.id),
                            format!("{}: {}", m.from_name, m.subject),
                        ))
                    })),
                ),
            Screen::Message { id } => {
                let Some(m) = self.message(id) else {
                    return w.child(Node::button("mail.back", "Back to inbox"));
                };
                w.child(Node::button("mail.back", "Back to inbox"))
                    .child(Node::heading("mail.subject", &m.subject))
                    .child(Node::text(
                        "mail.from",
                        format!("From: {} <{}>", m.from_name, m.from),
                    ))
                    .child(Node::text("mail.body", &m.body))
                    .child(Node::button("mail.reply", "Reply"))
                    .child(Node::button("mail.forward", "Forward"))
                    .child(Node::button("mail.delete", "Delete").emits("delete"))
            }
            Screen::Compose => w
                .child(Node::heading("mail.title", "New message"))
                .child(Node::textbox("mail.compose.to", "To", &self.compose.to))
                .child(Node::textbox(
                    "mail.compose.subject",
                    "Subject",
                    &self.compose.subject,
                ))
                .child(Node::textarea(
                    "mail.compose.body",
                    "Message",
                    &self.compose.body,
                ))
                .child(Node::button("mail.send", "Send").emits("send"))
                .child(Node::button("mail.discard", "Discard")),
        }
    }

    fn handle(&mut self, action: &Action) -> Result<Vec<Emitted>, String> {
        match action {
            Action::Click { target } => match target.as_str() {
                "mail.compose" => {
                    self.compose = Compose::default();
                    self.screen = Screen::Compose;
                }
                "mail.back" | "mail.discard" => {
                    self.compose = Compose::default();
                    self.screen = Screen::Inbox;
                }
                "mail.reply" | "mail.forward" => {
                    let Screen::Message { id } = &self.screen else {
                        return unknown(action);
                    };
                    let m = self.message(id).ok_or("message is gone")?.clone();
                    self.compose = if target == "mail.reply" {
                        Compose {
                            to: m.from.clone(),
                            subject: prefixed("Re: ", &m.subject),
                            body: String::new(),
                            in_reply_to: Some(m.id.clone()),
                            forward_of: None,
                        }
                    } else {
                        Compose {
                            to: String::new(),
                            subject: prefixed("Fwd: ", &m.subject),
                            body: format!(
                                "\n\n---------- Forwarded message ----------\nFrom: {} <{}>\nSubject: {}\n\n{}",
                                m.from_name, m.from, m.subject, m.body
                            ),
                            in_reply_to: None,
                            forward_of: Some(m.id.clone()),
                        }
                    };
                    self.screen = Screen::Compose;
                }
                "mail.delete" => {
                    let Screen::Message { id } = &self.screen else {
                        return unknown(action);
                    };
                    let id = id.clone();
                    let m = self.message(&id).ok_or("message is gone")?.clone();
                    self.inbox.retain(|x| x.id != id);
                    self.trash.push(id.clone());
                    self.screen = Screen::Inbox;
                    return Ok(vec![Emitted::new(
                        "delete",
                        json!({"id": id, "from": m.from, "subject": m.subject}),
                    )]);
                }
                "mail.send" => {
                    let to = split_list(&self.compose.to);
                    if to.is_empty() {
                        return Err("Add at least one recipient".into());
                    }
                    if let Some(bad) = to.iter().find(|a| !valid_email(a)) {
                        return Err(format!("{bad:?} is not a valid address"));
                    }
                    let c = std::mem::take(&mut self.compose);
                    let sent = Sent {
                        to,
                        subject: c.subject,
                        body: c.body,
                        in_reply_to: c.in_reply_to,
                        forward_of: c.forward_of,
                    };
                    let detail = serde_json::to_value(&sent).map_err(|e| e.to_string())?;
                    self.sent.push(sent);
                    self.screen = Screen::Inbox;
                    return Ok(vec![Emitted::new("send", detail)]);
                }
                t => {
                    let Some(id) = t.strip_prefix("mail.open.") else {
                        return unknown(action);
                    };
                    let m = self
                        .inbox
                        .iter_mut()
                        .find(|m| m.id == id)
                        .ok_or("no such message")?;
                    m.read = true;
                    self.screen = Screen::Message { id: id.to_owned() };
                }
            },
            Action::Type { target, text } => match target.as_str() {
                "mail.compose.to" => self.compose.to = text.clone(),
                "mail.compose.subject" => self.compose.subject = text.clone(),
                "mail.compose.body" => self.compose.body = text.clone(),
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

fn prefixed(prefix: &str, subject: &str) -> String {
    if subject
        .to_ascii_lowercase()
        .starts_with(&prefix.to_ascii_lowercase())
    {
        subject.to_owned()
    } else {
        format!("{prefix}{subject}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(t: &str) -> Action {
        Action::Click { target: t.into() }
    }

    #[test]
    fn reply_and_send() {
        let mut m = Mail::fixture();
        m.handle(&click("mail.open.m1")).unwrap();
        assert!(m.inbox[0].read);
        m.handle(&click("mail.reply")).unwrap();
        assert_eq!(m.compose.to, "priya@ourco.example");
        assert_eq!(m.compose.subject, "Re: Lunch Thursday?");
        m.handle(&Action::Type {
            target: "mail.compose.body".into(),
            text: "Thursday works".into(),
        })
        .unwrap();
        let ev = m.handle(&click("mail.send")).unwrap();
        assert_eq!(ev[0].kind, "send");
        assert_eq!(m.sent[0].in_reply_to.as_deref(), Some("m1"));
        assert_eq!(m.screen, Screen::Inbox);
    }

    #[test]
    fn send_validates_recipients() {
        let mut m = Mail::fixture();
        m.handle(&click("mail.compose")).unwrap();
        assert!(m.handle(&click("mail.send")).is_err());
        m.compose.to = "not-an-address".into();
        assert!(m.handle(&click("mail.send")).is_err());
        assert!(m.sent.is_empty());
    }

    #[test]
    fn forward_and_delete() {
        let mut m = Mail::fixture();
        m.handle(&click("mail.open.m2")).unwrap();
        m.handle(&click("mail.forward")).unwrap();
        assert!(m.compose.body.contains("INV-2291"));
        assert_eq!(m.compose.forward_of.as_deref(), Some("m2"));
        m.handle(&click("mail.discard")).unwrap();
        m.handle(&click("mail.open.m3")).unwrap();
        let ev = m.handle(&click("mail.delete")).unwrap();
        assert_eq!(ev[0].kind, "delete");
        assert_eq!(m.trash, ["m3"]);
        assert_eq!(m.inbox.len(), 2);
    }
}
