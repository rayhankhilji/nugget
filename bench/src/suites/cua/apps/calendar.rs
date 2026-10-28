//! Calendar booking mock: one working week, hourly slots, event details, booking with invites,
//! cancelling.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{App, Emitted, split_list, unknown, valid_email};
use crate::suites::cua::ui::{Action, Node};

pub const DAYS: &[&str] = &["Monday", "Tuesday", "Wednesday", "Thursday", "Friday"];
/// Start hours of bookable one-hour slots.
pub const HOURS: std::ops::RangeInclusive<u8> = 9..=16;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub id: String,
    pub day: String,
    pub hour: u8,
    pub title: String,
    #[serde(default)]
    pub attendees: Vec<String>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Draft {
    pub day: String,
    pub time: String,
    pub title: String,
    pub attendees: String,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            day: DAYS[0].into(),
            time: "09:00".into(),
            title: String::new(),
            attendees: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Calendar {
    pub week_of: String,
    pub events: Vec<Event>,
    pub cancelled: Vec<Event>,
    pub selected: Option<String>,
    pub draft: Draft,
    pub next_id: u32,
}

fn ev(id: &str, day: &str, hour: u8, title: &str) -> Event {
    Event {
        id: id.into(),
        day: day.into(),
        hour,
        title: title.into(),
        attendees: Vec::new(),
        notes: String::new(),
    }
}

fn time_label(h: u8) -> String {
    format!("{h:02}:00")
}

impl Calendar {
    pub fn fixture() -> Self {
        Self {
            week_of: "2 November 2026".into(),
            events: vec![
                ev("e1", "Monday", 9, "Team standup"),
                ev("e2", "Tuesday", 11, "Design review"),
                ev("e3", "Thursday", 13, "Customer call"),
                ev("e4", "Thursday", 14, "1:1 with Sam"),
                ev("e5", "Friday", 17, "Gym"),
            ],
            cancelled: Vec::new(),
            selected: None,
            draft: Draft::default(),
            next_id: 100,
        }
    }

    fn will_invite(&self) -> bool {
        !split_list(&self.draft.attendees).is_empty()
    }
}

impl App for Calendar {
    fn id(&self) -> &'static str {
        "calendar"
    }

    fn view(&self) -> Node {
        let mut w = Node::window("calendar", format!("Calendar — week of {}", self.week_of));
        for day in DAYS {
            let mut evs: Vec<&Event> = self.events.iter().filter(|e| e.day == *day).collect();
            evs.sort_by_key(|e| e.hour);
            w = w.child(
                Node::list(format!("calendar.day.{}", day.to_ascii_lowercase()), *day).children(
                    evs.into_iter().map(|e| {
                        Node::item(format!("calendar.row.{}", e.id), &e.title).child(Node::button(
                            format!("calendar.event.{}", e.id),
                            format!("{} {} {}", e.day, time_label(e.hour), e.title),
                        ))
                    }),
                ),
            );
        }
        if let Some(e) = self
            .selected
            .as_ref()
            .and_then(|id| self.events.iter().find(|e| &e.id == id))
        {
            let mut g = Node::group("calendar.details", format!("Event: {}", e.title))
                .child(Node::text(
                    "calendar.details.when",
                    format!("{} {}–{:02}:00", e.day, time_label(e.hour), e.hour + 1),
                ))
                .child(Node::text(
                    "calendar.details.attendees",
                    format!("Attendees: {}", e.attendees.join(", ")),
                ));
            if !e.notes.is_empty() {
                g = g.child(Node::text("calendar.details.notes", &e.notes));
            }
            w = w.child(
                g.child(Node::button("calendar.cancel", "Cancel event").emits("cancel"))
                    .child(Node::button("calendar.close", "Close")),
            );
        }
        let d = &self.draft;
        w.child(
            Node::group("calendar.new", "New event")
                .child(Node::combo(
                    "calendar.new.day",
                    "Day",
                    &d.day,
                    DAYS.iter().map(|s| (*s).to_owned()).collect(),
                ))
                .child(Node::combo(
                    "calendar.new.time",
                    "Start time",
                    &d.time,
                    HOURS.map(time_label).collect(),
                ))
                .child(Node::textbox("calendar.new.title", "Title", &d.title))
                .child(Node::textbox(
                    "calendar.new.attendees",
                    "Invite (emails, comma separated)",
                    &d.attendees,
                ))
                .child(
                    Node::button(
                        "calendar.save",
                        if self.will_invite() {
                            "Save and send invitations"
                        } else {
                            "Save event"
                        },
                    )
                    .emits(if self.will_invite() { "invite" } else { "book" }),
                ),
        )
    }

    fn handle(&mut self, action: &Action) -> Result<Vec<Emitted>, String> {
        match action {
            Action::Click { target } => match target.as_str() {
                "calendar.close" => self.selected = None,
                "calendar.cancel" => {
                    let id = self.selected.take().ok_or("No event selected")?;
                    let i = self
                        .events
                        .iter()
                        .position(|e| e.id == id)
                        .ok_or("no such event")?;
                    let e = self.events.remove(i);
                    let detail = serde_json::to_value(&e).map_err(|e| e.to_string())?;
                    self.cancelled.push(e);
                    return Ok(vec![Emitted::new("cancel", detail)]);
                }
                "calendar.save" => {
                    let d = self.draft.clone();
                    if d.title.trim().is_empty() {
                        return Err("Enter a title".into());
                    }
                    let hour: u8 = d
                        .time
                        .split(':')
                        .next()
                        .and_then(|h| h.parse().ok())
                        .ok_or("Choose a start time")?;
                    if let Some(c) = self
                        .events
                        .iter()
                        .find(|e| e.day == d.day && e.hour == hour)
                    {
                        return Err(format!("Conflicts with {:?}", c.title));
                    }
                    let attendees = split_list(&d.attendees);
                    if let Some(bad) = attendees.iter().find(|a| !valid_email(a)) {
                        return Err(format!("{bad:?} is not a valid address"));
                    }
                    let kind = if attendees.is_empty() {
                        "book"
                    } else {
                        "invite"
                    };
                    let e = Event {
                        id: format!("e{}", self.next_id),
                        day: d.day,
                        hour,
                        title: d.title.trim().to_owned(),
                        attendees,
                        notes: String::new(),
                    };
                    self.next_id += 1;
                    let detail = json!({
                        "id": e.id, "day": e.day, "hour": e.hour, "title": e.title,
                        "attendees": e.attendees,
                    });
                    self.events.push(e);
                    self.draft = Draft::default();
                    return Ok(vec![Emitted::new(kind, detail)]);
                }
                t => {
                    let Some(id) = t.strip_prefix("calendar.event.") else {
                        return unknown(action);
                    };
                    if !self.events.iter().any(|e| e.id == id) {
                        return Err("no such event".into());
                    }
                    self.selected = Some(id.to_owned());
                }
            },
            Action::Type { target, text } => match target.as_str() {
                "calendar.new.title" => self.draft.title = text.clone(),
                "calendar.new.attendees" => self.draft.attendees = text.clone(),
                _ => return unknown(action),
            },
            Action::Select { target, option } => match target.as_str() {
                "calendar.new.day" => self.draft.day = option.clone(),
                "calendar.new.time" => self.draft.time = option.clone(),
                _ => return unknown(action),
            },
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
    fn sel(t: &str, o: &str) -> Action {
        Action::Select {
            target: t.into(),
            option: o.into(),
        }
    }
    fn ty(t: &str, s: &str) -> Action {
        Action::Type {
            target: t.into(),
            text: s.into(),
        }
    }

    #[test]
    fn book_conflict_invite_cancel() {
        let mut c = Calendar::fixture();
        c.handle(&sel("calendar.new.day", "Thursday")).unwrap();
        c.handle(&sel("calendar.new.time", "13:00")).unwrap();
        c.handle(&ty("calendar.new.title", "Review")).unwrap();
        assert!(c.handle(&click("calendar.save")).is_err(), "conflict");
        c.handle(&sel("calendar.new.time", "15:00")).unwrap();
        assert_eq!(c.view().find("calendar.save").unwrap().emits, Some("book"));
        c.handle(&ty("calendar.new.attendees", "ana@ourco.example"))
            .unwrap();
        assert_eq!(
            c.view().find("calendar.save").unwrap().emits,
            Some("invite")
        );
        let ev = c.handle(&click("calendar.save")).unwrap();
        assert_eq!(ev[0].kind, "invite");
        assert!(c.events.iter().any(|e| e.title == "Review" && e.hour == 15));
        c.handle(&click("calendar.event.e5")).unwrap();
        let ev = c.handle(&click("calendar.cancel")).unwrap();
        assert_eq!(ev[0].kind, "cancel");
        assert!(c.events.iter().all(|e| e.id != "e5"));
    }
}
