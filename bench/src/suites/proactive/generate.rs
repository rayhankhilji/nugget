//! Builds the fixture world and its gold labels. Deterministic: no seed parameter, because the
//! world is a committed fixture; `--seed` only drives the simulated user.
//!
//! Gold items (unanswered threads, follow-ups, commitments, meetings to prepare, renewals) are
//! written out by hand below, with the day each becomes due. Distractors and neutral traffic
//! (newsletters, notifications, bulk mail, answered threads, cancelled meetings, spam renewals,
//! FYI mail, routine meetings, notes and one-off receipts) are generated from templates around
//! them.
//!
//! Items are built with provisional ids, sorted by day, and then renumbered (`m-0001`, `e-001`,
//! `t-001`, ...), so ids follow the timeline.

// The data tables below are tuples of literals; naming their types would not help readers.
#![allow(clippy::type_complexity)]

use std::collections::HashMap;

use crate::corpus::Rng;

use super::world::{
    Cadence, CardKind, Distractor, DistractorCategory, Event, EventCancelled, Gold, GoldCard, Item,
    Mail, Note, Person, Receipt, World, commit_subject, event_subject, iso_date, pretty_date,
    sub_subject, thread_subject, weekday,
};

pub const USER: &str = "sam@example.com";

const PRIYA: &str = "priya@example.com";
const MARCO: &str = "marco@example.com";
const LENA: &str = "lena@example.com";
const JONAS: &str = "jonas@example.com";
const AISHA: &str = "aisha@example.com";
const TOM: &str = "tom@example.com";
const DANA: &str = "dana@example.com";
const KIM: &str = "kim@northwind.test";
const RAJ: &str = "raj@acme.test";
const OLGA: &str = "olga@fabrikam.test";
const BEN: &str = "ben@contoso.test";
const MEI: &str = "mei@globex.test";
const LUIS: &str = "luis@initech.test";
const NADIA: &str = "nadia@tailspin.test";
const OMAR: &str = "omar@woodgrove.test";
const CAROL: &str = "carol@rivera-family.test";
const ALEX: &str = "alex@mailbox.test";

const COLLEAGUES: [&str; 7] = [PRIYA, MARCO, LENA, JONAS, AISHA, TOM, DANA];
const CONTACTS: [&str; 6] = [KIM, RAJ, OLGA, NADIA, OMAR, CAROL];

/// A subject before ids are final.
#[derive(Clone)]
enum Subj {
    Thread(String),
    Event(String),
    Commit(String),
    Sub(String),
}

struct GoldDraft {
    kind: CardKind,
    subject: Subj,
    first_day: i32,
    last_day: i32,
    source: String,
    why: String,
}

#[derive(Default)]
struct Gen {
    seq: usize,
    threads: usize,
    items: Vec<Item>,
    gold: Vec<GoldDraft>,
    distractors: Vec<(String, DistractorCategory, Vec<Subj>)>,
}

impl Gen {
    fn pid(&mut self) -> String {
        self.seq += 1;
        format!("p{}", self.seq)
    }

    fn thread(&mut self) -> String {
        self.threads += 1;
        format!("pt{}", self.threads)
    }

    #[allow(clippy::too_many_arguments)]
    fn mail_full(
        &mut self,
        day: i32,
        thread: &str,
        from: &str,
        to: &[&str],
        subject: &str,
        body: &str,
        list_unsubscribe: bool,
        auto_submitted: bool,
    ) -> String {
        let id = self.pid();
        self.items.push(Item::Mail(Mail {
            id: id.clone(),
            day,
            thread: thread.to_owned(),
            from: from.to_owned(),
            to: to.iter().map(|s| (*s).to_owned()).collect(),
            subject: subject.to_owned(),
            body: body.to_owned(),
            list_unsubscribe,
            auto_submitted,
        }));
        id
    }

    fn mail(
        &mut self,
        day: i32,
        thread: &str,
        from: &str,
        to: &[&str],
        subject: &str,
        body: &str,
    ) -> String {
        self.mail_full(day, thread, from, to, subject, body, false, false)
    }

    /// A new single-message thread. Returns `(mail id, thread id)`.
    fn single(
        &mut self,
        day: i32,
        from: &str,
        to: &[&str],
        subject: &str,
        body: &str,
    ) -> (String, String) {
        let t = self.thread();
        (self.mail(day, &t, from, to, subject, body), t)
    }

    #[allow(clippy::too_many_arguments)]
    fn event(
        &mut self,
        day: i32,
        start_day: i32,
        start_hour: u8,
        minutes: u16,
        title: &str,
        organizer: &str,
        attendees: &[&str],
        series: Option<&str>,
        location: &str,
        description: &str,
    ) -> String {
        let id = self.pid();
        self.items.push(Item::Event(Event {
            id: id.clone(),
            day,
            start_day,
            start_hour,
            minutes,
            title: title.to_owned(),
            organizer: organizer.to_owned(),
            attendees: attendees.iter().map(|s| (*s).to_owned()).collect(),
            series: series.map(str::to_owned),
            location: location.to_owned(),
            description: description.to_owned(),
        }));
        id
    }

    fn note(&mut self, day: i32, title: &str, body: &str) -> String {
        let id = self.pid();
        self.items.push(Item::Note(Note {
            id: id.clone(),
            day,
            title: title.to_owned(),
            body: body.to_owned(),
        }));
        id
    }

    #[allow(clippy::too_many_arguments)]
    fn receipt(
        &mut self,
        day: i32,
        merchant: &str,
        domain: &str,
        amount_cents: u32,
        description: &str,
        cadence: Option<Cadence>,
        renews_on: Option<i32>,
    ) -> String {
        let id = self.pid();
        self.items.push(Item::Receipt(Receipt {
            id: id.clone(),
            day,
            merchant: merchant.to_owned(),
            merchant_domain: domain.to_owned(),
            amount_cents,
            description: description.to_owned(),
            cadence,
            renews_on,
        }));
        id
    }

    fn gold(&mut self, kind: CardKind, subject: Subj, days: (i32, i32), source: &str, why: &str) {
        self.gold.push(GoldDraft {
            kind,
            subject,
            first_day: days.0,
            last_day: days.1,
            source: source.to_owned(),
            why: why.to_owned(),
        });
    }

    fn distractor(&mut self, item: &str, category: DistractorCategory, subjects: Vec<Subj>) {
        self.distractors.push((item.to_owned(), category, subjects));
    }
}

fn name_of(addr: &str) -> String {
    let local = addr.split('@').next().unwrap_or(addr);
    let mut c = local.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Unanswered: someone asks the user something on `day` and never hears back. Due from two days
/// later (a day's grace) for six days.
const UNANSWERED: [(i32, &str, &str, &str); 13] = [
    (
        2,
        PRIYA,
        "Q3 budget numbers",
        "Hi Sam, could you send me the final Q3 numbers before the planning meeting? Finance is asking.",
    ),
    (
        3,
        KIM,
        "Contract renewal terms",
        "Hello Sam, can you confirm whether the 12-month term works for your side? We would like to lock it in this month.",
    ),
    (
        5,
        BEN,
        "Intro: Contoso evaluation",
        "Hi Sam, Nadia suggested I reach out. Would you have time for a short demo next week? Happy to work around your calendar.",
    ),
    (
        7,
        DANA,
        "Second backend role",
        "What do you think about opening the second backend role now rather than in Q1? I'd like your view before Friday.",
    ),
    (
        9,
        RAJ,
        "Sandbox credentials",
        "Hi Sam, can you share how we request sandbox credentials for the integration? Our team is blocked on it.",
    ),
    (
        11,
        CAROL,
        "Thanksgiving plans",
        "Sweetheart, are you coming home for Thanksgiving this year? Let me know so I can plan the food.",
    ),
    (
        13,
        MARCO,
        "Design review feedback",
        "Sam, could you look at the onboarding mockups and tell me which variant you prefer? I need to pick one this week.",
    ),
    (
        15,
        MEI,
        "Partnership question",
        "Hi Sam, we met at the meetup. Would Example Co be open to a joint pilot with Globex in Q4?",
    ),
    (
        17,
        LENA,
        "On-call swap",
        "Can you swap on-call with me on the 26th? I have a family thing that weekend.",
    ),
    (
        19,
        OLGA,
        "Invoice 4471",
        "Hello, could you check invoice 4471? The amount differs from the purchase order by 300.",
    ),
    (
        21,
        ALEX,
        "Hiking on Saturday",
        "Are you in for the ridge trail on Saturday? Need to know by Thursday to book the shuttle.",
    ),
    (
        22,
        JONAS,
        "Launch checklist owners",
        "Sam, can you confirm you own the docs and the migration items on the launch checklist?",
    ),
    (
        23,
        LUIS,
        "Speaking slot at Initech day",
        "Hi Sam, would you be willing to give a 20 minute talk at our customer day in October?",
    ),
];

/// Answered: a question, then the user's reply on the same day (`0`) or the next (`1`).
const ANSWERED: [(i32, &str, &str, &str, &str, i32); 18] = [
    (
        1,
        MARCO,
        "Standup time",
        "Can we move standup to 9:45 this week?",
        "Sure, 9:45 works for me.",
        0,
    ),
    (
        2,
        KIM,
        "Slides from Tuesday",
        "Could you send over the slides from Tuesday?",
        "Here they are, attached.",
        0,
    ),
    (
        3,
        AISHA,
        "Lunch order",
        "Do you want the usual from the deli?",
        "Yes please, the usual.",
        0,
    ),
    (
        4,
        RAJ,
        "Webhook retries",
        "Do your webhooks retry on 503s?",
        "They do, with exponential backoff up to an hour.",
        1,
    ),
    (
        6,
        TOM,
        "Laptop refresh",
        "Which laptop model do you want in the refresh?",
        "The 14 inch one, thanks.",
        0,
    ),
    (
        8,
        CAROL,
        "Photos",
        "Did you get the photos from the lake?",
        "Got them, they're lovely!",
        1,
    ),
    (
        9,
        NADIA,
        "Referral",
        "Is it okay if I give Ben your address?",
        "Of course, go ahead.",
        0,
    ),
    (
        10,
        PRIYA,
        "Headcount sheet",
        "Can you check row 14 of the headcount sheet?",
        "Fixed row 14, it was a typo.",
        1,
    ),
    (
        12,
        OMAR,
        "Quote validity",
        "Can you confirm the quote is valid until end of month?",
        "Confirmed, valid until the 30th.",
        0,
    ),
    (
        14,
        LENA,
        "Runbook link",
        "Where is the runbook for the cache failover?",
        "It's in the ops wiki under Caching.",
        0,
    ),
    (
        15,
        JONAS,
        "Feature flag",
        "Can I turn on the new search flag for staff?",
        "Yes, go ahead for staff only.",
        1,
    ),
    (
        16,
        ALEX,
        "Book recommendation",
        "What was the name of that novel you mentioned?",
        "The Glass Orchard. You'll love it.",
        0,
    ),
    (
        18,
        DANA,
        "1:1 agenda",
        "Anything you want to add to the 1:1 agenda?",
        "Just the hiring plan, thanks.",
        0,
    ),
    (
        20,
        KIM,
        "Pilot metrics",
        "Could you share the pilot metrics we discussed?",
        "Shared the dashboard link with you.",
        1,
    ),
    (
        22,
        MARCO,
        "Icon set",
        "Do we have a licence for the new icon set?",
        "Yes, the team licence covers it.",
        0,
    ),
    (
        24,
        OLGA,
        "Delivery address",
        "Should the hardware ship to the main office?",
        "Yes, main office, attention Sam.",
        0,
    ),
    (
        26,
        AISHA,
        "Offsite dates",
        "Do the 14th and 15th work for the offsite?",
        "Both work for me.",
        1,
    ),
    (
        27,
        PRIYA,
        "Review slot",
        "Can you review my PR before Thursday?",
        "Reviewed and approved.",
        0,
    ),
];

/// Follow-ups: the user asks on `day` and never hears back. Due from four days later for six days.
const FOLLOW_UPS: [(i32, &str, &str, &str); 8] = [
    (
        1,
        OMAR,
        "Hardware quote",
        "Hi Omar, could you send an updated quote for the 20 laptops? Thanks, Sam",
    ),
    (
        4,
        NADIA,
        "Case study draft",
        "Hi Nadia, did you get a chance to look at the case study draft? Any edits?",
    ),
    (
        6,
        KIM,
        "Pilot data export",
        "Kim, could your team send the pilot data export so we can finish the analysis?",
    ),
    (
        10,
        RAJ,
        "API rate limits",
        "Raj, can you confirm the rate limits we should plan for in production?",
    ),
    (
        12,
        OLGA,
        "Support contract",
        "Olga, could you send the renewed support contract for signature?",
    ),
    (
        16,
        DANA,
        "Budget approval",
        "Dana, did finance approve the tooling budget? I want to place the order.",
    ),
    (
        18,
        BEN,
        "Security questionnaire",
        "Ben, could you send back the completed security questionnaire?",
    ),
    (
        20,
        CAROL,
        "Flight times",
        "Mom, which flight are you on? I'll pick you up.",
    ),
];

/// Answered follow-ups: the user asks on `day`, the other side replies `delay` days later.
const ANSWERED_FOLLOW_UPS: [(i32, &str, &str, &str, &str, i32); 6] = [
    (
        2,
        LENA,
        "Pager rotation",
        "Lena, can you add me to the pager rotation from next month?",
        "Done, you're on from the 1st.",
        1,
    ),
    (
        5,
        OMAR,
        "Monitor stands",
        "Omar, do you stock monitor arms as well?",
        "We do, I've added them to the quote.",
        2,
    ),
    (
        8,
        MARCO,
        "Brand colours",
        "Marco, what's the final hex for the new primary colour?",
        "It's #2F5DFF.",
        1,
    ),
    (
        13,
        NADIA,
        "Logo usage",
        "Nadia, may we use your logo on our customers page?",
        "Yes, approved by marketing.",
        3,
    ),
    (
        17,
        JONAS,
        "Migration window",
        "Jonas, which night works for the migration?",
        "Wednesday night works.",
        2,
    ),
    (
        21,
        RAJ,
        "Status page",
        "Raj, do you have a status page we can subscribe to?",
        "Yes: status.acme.test.",
        1,
    ),
];

/// Open commitments made in mail: a request on `day`, the user's promise the same day, due `due`.
const COMMITMENTS: [(i32, &str, &str, &str, &str, i32); 5] = [
    (
        2,
        DANA,
        "Quarterly review deck",
        "Could you put together the quarterly review deck?",
        "the quarterly review deck",
        9,
    ),
    (
        5,
        KIM,
        "Integration guide",
        "Can you send us the integration guide for our engineers?",
        "the integration guide",
        12,
    ),
    (
        11,
        PRIYA,
        "Hiring scorecard",
        "Could you draft the scorecard for the backend role?",
        "the hiring scorecard",
        18,
    ),
    (
        14,
        RAJ,
        "Joint roadmap",
        "Can you share your side of the joint roadmap?",
        "our side of the joint roadmap",
        22,
    ),
    (
        19,
        MARCO,
        "Copy for onboarding",
        "Could you write the copy for the onboarding screens?",
        "the onboarding copy",
        26,
    ),
];

/// Commitments the user delivered early: promise on `day`, delivered on `done`, due `due`.
const COMPLETED: [(i32, &str, &str, &str, &str, i32, i32); 3] = [
    (
        6,
        OLGA,
        "Signed NDA",
        "Could you return the signed NDA?",
        "the signed NDA",
        8,
        15,
    ),
    (
        8,
        AISHA,
        "Offsite budget",
        "Can you send me the offsite budget estimate?",
        "the offsite budget estimate",
        10,
        14,
    ),
    (
        15,
        TOM,
        "Asset inventory",
        "Could you send the asset inventory for your team?",
        "the asset inventory",
        17,
        24,
    ),
];

/// Commitments in notes: written on `day`, due `due`.
const NOTE_COMMITMENTS: [(i32, &str, &str, i32); 3] = [
    (
        3,
        "Personal admin",
        "Send the landlord the signed lease extension",
        10,
    ),
    (13, "Work todos", "Submit the conference talk proposal", 20),
    (
        20,
        "Personal admin",
        "Renew passport photos and post the application",
        28,
    ),
];

/// External meetings to prepare for: invite on `invite`, meeting on `start`.
const MEETINGS: [(i32, i32, u8, &str, &str, &[&str], &str); 8] = [
    (
        -2,
        4,
        14,
        "Northwind quarterly business review",
        KIM,
        &[KIM, USER, DANA],
        "Review pilot results and Q4 plan.",
    ),
    (
        3,
        8,
        10,
        "Contoso product demo",
        BEN,
        &[BEN, USER, NADIA],
        "Demo for Contoso's evaluation team.",
    ),
    (
        5,
        10,
        15,
        "Acme integration sync",
        RAJ,
        &[RAJ, USER, JONAS],
        "Unblock the sandbox and agree on rate limits.",
    ),
    (
        8,
        14,
        11,
        "Fabrikam vendor review",
        OLGA,
        &[OLGA, USER],
        "Support contract and invoice discrepancies.",
    ),
    (
        12,
        17,
        16,
        "Globex partnership call",
        MEI,
        &[MEI, USER, DANA],
        "Explore a joint pilot.",
    ),
    (
        16,
        22,
        13,
        "Northwind contract negotiation",
        KIM,
        &[KIM, USER, DANA],
        "Term length and pricing.",
    ),
    (
        19,
        25,
        10,
        "Initech customer day planning",
        LUIS,
        &[LUIS, USER],
        "Talk topic and logistics.",
    ),
    (
        23,
        29,
        9,
        "Investor update with Globex",
        MEI,
        &[MEI, USER, DANA, PRIYA],
        "Quarterly numbers and hiring plan.",
    ),
];

/// Meetings cancelled two days before they start: invite on `invite`, start on `start`.
const CANCELLED: [(i32, i32, u8, &str, &str, &str); 5] = [
    (
        1,
        7,
        15,
        "Woodgrove hardware walkthrough",
        OMAR,
        "Omar is travelling",
    ),
    (
        4,
        11,
        10,
        "Tailspin case study interview",
        NADIA,
        "Moving to async questions",
    ),
    (
        8,
        15,
        14,
        "Contoso security review",
        BEN,
        "Questionnaire covers it",
    ),
    (
        16,
        24,
        11,
        "Acme roadmap workshop",
        RAJ,
        "Postponed to next quarter",
    ),
    (
        20,
        28,
        16,
        "Fabrikam onboarding call",
        OLGA,
        "No longer needed",
    ),
];

/// Annual subscriptions renewing inside the month: (merchant, domain, cents, renews on).
const ANNUAL_RENEWALS: [(&str, &str, u32, i32); 5] = [
    ("CloudVault", "cloudvault.test", 9_900, 8),
    ("PassKeeper", "passkeeper.test", 3_600, 12),
    ("DomainHost", "domainhost.test", 1_500, 18),
    ("NewsReader Pro", "newsreader.test", 4_800, 23),
    ("FitLog", "fitlog.test", 5_999, 27),
];

/// Monthly subscriptions: renewing inside the month, but the user does not want reminders.
const MONTHLY: [(&str, &str, u32, i32); 3] = [
    ("Streamly", "streamly.test", 999, 6),
    ("MusicBox", "musicbox.test", 1_099, 16),
    ("PhotoCloud", "photocloud.test", 299, 26),
];

pub struct Fixture {
    pub world: World,
    pub gold: Gold,
}

/// Build the world and gold labels.
pub fn generate() -> Fixture {
    let mut g = Gen::default();
    // Generator noise only (template choices). Fixed: the world is a committed fixture.
    let mut rng = Rng::new(0x5EED_2026);

    history(&mut g);
    gold_mail(&mut g);
    meetings(&mut g);
    renewals(&mut g);
    newsletters(&mut g, &mut rng);
    notifications(&mut g, &mut rng);
    bulk(&mut g);
    spam(&mut g);
    fyi(&mut g, &mut rng);
    notes(&mut g);
    finish(g)
}

/// Mail the user sent before day 1, so their usual correspondents are known.
fn history(g: &mut Gen) {
    let people: [(&str, &str); 10] = [
        (DANA, "Notes from our planning session"),
        (PRIYA, "Budget template"),
        (MARCO, "Onboarding research summary"),
        (KIM, "Welcome to the pilot"),
        (RAJ, "Integration kickoff notes"),
        (OLGA, "Purchase order 2210"),
        (CAROL, "Visiting in November"),
        (ALEX, "Trail map"),
        (LENA, "Ops handover"),
        (JONAS, "Launch timeline"),
    ];
    for (i, (to, subject)) in people.iter().enumerate() {
        let day = -14 + i as i32;
        g.single(
            day,
            USER,
            &[to],
            subject,
            &format!("Hi {}, sharing this for reference. Best, Sam", name_of(to)),
        );
    }
}

fn gold_mail(g: &mut Gen) {
    for (day, from, subject, body) in UNANSWERED {
        let (id, t) = g.single(day, from, &[USER], subject, body);
        g.gold(
            CardKind::UnansweredThread,
            Subj::Thread(t),
            (day + 2, day + 7),
            &id,
            &format!(
                "{} asked a question and is waiting for a reply",
                name_of(from)
            ),
        );
    }
    for (day, from, subject, q, a, delay) in ANSWERED {
        let (id, t) = g.single(day, from, &[USER], subject, q);
        g.mail(day + delay, &t, USER, &[from], &format!("Re: {subject}"), a);
        g.distractor(
            &id,
            DistractorCategory::AnsweredThread,
            vec![Subj::Thread(t)],
        );
    }
    for (day, to, subject, body) in FOLLOW_UPS {
        let (id, t) = g.single(day, USER, &[to], subject, body);
        g.gold(
            CardKind::FollowUp,
            Subj::Thread(t),
            (day + 4, day + 9),
            &id,
            &format!("Sam asked {} and has had no answer", name_of(to)),
        );
    }
    for (day, to, subject, q, a, delay) in ANSWERED_FOLLOW_UPS {
        let (id, t) = g.single(day, USER, &[to], subject, q);
        g.mail(day + delay, &t, to, &[USER], &format!("Re: {subject}"), a);
        g.distractor(
            &id,
            DistractorCategory::AnsweredThread,
            vec![Subj::Thread(t)],
        );
    }
    for (day, from, subject, ask, what, due) in COMMITMENTS {
        let (req, t) = g.single(
            day,
            from,
            &[USER],
            subject,
            &format!("Hi Sam, {}", lower_first(ask)),
        );
        let promise = g.mail(
            day,
            &t,
            USER,
            &[from],
            &format!("Re: {subject}"),
            &format!("Happy to. I'll send {what} by {}.", iso_date(due)),
        );
        g.gold(
            CardKind::CommitmentDue,
            Subj::Commit(promise.clone()),
            (due - 2, due),
            &promise,
            &format!("Sam promised {what} by {}", pretty_date(due)),
        );
        g.distractor(
            &req,
            DistractorCategory::AnsweredThread,
            vec![Subj::Thread(t)],
        );
    }
    for (day, from, subject, ask, what, done, due) in COMPLETED {
        let (_, t) = g.single(
            day,
            from,
            &[USER],
            subject,
            &format!("Hi Sam, {}", lower_first(ask)),
        );
        let promise = g.mail(
            day,
            &t,
            USER,
            &[from],
            &format!("Re: {subject}"),
            &format!("Sure. I'll send {what} by {}.", iso_date(due)),
        );
        g.mail(
            done,
            &t,
            USER,
            &[from],
            &format!("Re: {subject}"),
            &format!("Attached is {what}, as promised."),
        );
        g.distractor(
            &promise,
            DistractorCategory::CompletedCommitment,
            vec![Subj::Commit(promise.clone()), Subj::Thread(t)],
        );
    }
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn meetings(g: &mut Gen) {
    for (invite, start, hour, title, organizer, attendees, desc) in MEETINGS {
        let id = g.event(
            invite,
            start,
            hour,
            60,
            title,
            organizer,
            attendees,
            None,
            "Video call",
            desc,
        );
        g.gold(
            CardKind::MeetingPrep,
            Subj::Event(id.clone()),
            (start - 1, start),
            &id,
            &format!("External meeting on {}", pretty_date(start)),
        );
    }
    // Agendas for three of them: context, not questions.
    for (day, from, subject) in [
        (2, KIM, "Agenda for the QBR"),
        (13, MEI, "Agenda: partnership call"),
        (20, KIM, "Agenda for contract negotiation"),
    ] {
        g.single(
            day,
            from,
            &[USER],
            subject,
            "Sharing the agenda ahead of our meeting. Talk soon.",
        );
    }
    for (invite, start, hour, title, organizer, reason) in CANCELLED {
        let id = g.event(
            invite,
            start,
            hour,
            45,
            title,
            organizer,
            &[organizer, USER],
            None,
            "Video call",
            "Discussion.",
        );
        let cid = g.pid();
        g.items.push(Item::EventCancelled(EventCancelled {
            id: cid,
            day: start - 2,
            event: id.clone(),
            reason: reason.to_owned(),
        }));
        g.single(
            start - 2,
            organizer,
            &[USER],
            &format!("Cancelled: {title}"),
            &format!("{reason}, so I've cancelled our meeting. Thanks for understanding."),
        );
        g.distractor(
            &id,
            DistractorCategory::CancelledMeeting,
            vec![Subj::Event(id.clone())],
        );
    }
    // Routine internal meetings: weekly team sync (Mondays) and a 1:1 with Dana (Wednesdays).
    for day in 1..=30 {
        match weekday(day) {
            0 => {
                g.event(
                    -10,
                    day,
                    10,
                    30,
                    "Team sync",
                    DANA,
                    &[DANA, USER, PRIYA, MARCO, LENA, JONAS],
                    Some("series-team-sync"),
                    "Room 3",
                    "Weekly status.",
                );
            }
            2 => {
                g.event(
                    -10,
                    day,
                    15,
                    30,
                    "1:1 Dana / Sam",
                    DANA,
                    &[DANA, USER],
                    Some("series-1on1"),
                    "Dana's office",
                    "Weekly 1:1.",
                );
            }
            _ => {}
        }
    }
    for (invite, start) in [(1, 3), (20, 24)] {
        g.event(
            invite,
            start,
            16,
            60,
            "All hands",
            TOM,
            &[TOM, USER, DANA, PRIYA, MARCO, LENA, JONAS, AISHA],
            None,
            "Auditorium",
            "Company update.",
        );
    }
    // Personal blocks without attendees.
    for (day, start, hour, title) in [
        (2, 10, 8, "Dentist"),
        (4, 12, 9, "Gym"),
        (9, 19, 9, "Gym"),
        (14, 26, 9, "Gym"),
        (18, 21, 18, "Car service"),
        (6, 9, 13, "Focus: planning"),
        (10, 16, 13, "Focus: planning"),
        (17, 23, 13, "Focus: planning"),
        (22, 30, 13, "Focus: planning"),
    ] {
        g.event(day, start, hour, 60, title, USER, &[], None, "", "");
    }
    // One-off internal meetings: routine, nothing to prepare.
    for (invite, start, title, with) in [
        (3, 6, "Coffee with Priya", PRIYA),
        (12, 18, "Design review: onboarding", MARCO),
        (19, 27, "Lunch and learn: observability", LENA),
    ] {
        g.event(
            invite,
            start,
            11,
            45,
            title,
            with,
            &[with, USER],
            None,
            "Office",
            "",
        );
    }
    g.event(8, 12, 10, 60, "Book club", USER, &[], None, "Library", "");
    g.event(14, 19, 16, 45, "Haircut", USER, &[], None, "", "");
    // Social plans with external people: not meetings to prepare for.
    g.event(
        9,
        13,
        12,
        90,
        "Lunch with Alex",
        ALEX,
        &[ALEX, USER],
        None,
        "Cafe Verde",
        "",
    );
    g.event(
        15,
        20,
        19,
        60,
        "Call with Mom",
        CAROL,
        &[CAROL, USER],
        None,
        "Phone",
        "",
    );
}

fn renewals(g: &mut Gen) {
    for (merchant, domain, cents, renews) in ANNUAL_RENEWALS {
        let r = g.receipt(
            renews - 365,
            merchant,
            domain,
            cents,
            &format!("{merchant} annual plan"),
            Some(Cadence::Annual),
            Some(renews),
        );
        g.single(
            renews - 8,
            &format!("billing@{domain}"),
            &[USER],
            &format!("Your {merchant} plan renews on {}", pretty_date(renews)),
            &format!(
                "Your annual {merchant} plan renews on {} for ${}.{:02}. No action is needed to keep it.",
                iso_date(renews),
                cents / 100,
                cents % 100
            ),
        );
        g.gold(
            CardKind::Renewal,
            Subj::Sub(domain.to_owned()),
            (renews - 7, renews - 1),
            &r,
            &format!("{merchant} annual plan renews on {}", pretty_date(renews)),
        );
    }
    for (merchant, domain, cents, renews) in MONTHLY {
        g.receipt(
            renews - 30,
            merchant,
            domain,
            cents,
            &format!("{merchant} monthly plan"),
            Some(Cadence::Monthly),
            Some(renews),
        );
        g.receipt(
            renews,
            merchant,
            domain,
            cents,
            &format!("{merchant} monthly plan"),
            Some(Cadence::Monthly),
            Some(renews + 30),
        );
    }
    // Annual plans renewing well after the month: never due inside it.
    for (merchant, domain, cents, renews) in [
        ("MapMaker", "mapmaker.test", 2_900, 60),
        ("TaxEasy", "taxeasy.test", 7_900, 120),
    ] {
        g.receipt(
            renews - 365,
            merchant,
            domain,
            cents,
            &format!("{merchant} annual plan"),
            Some(Cadence::Annual),
            Some(renews),
        );
    }
    for (day, merchant, domain, cents, what) in [
        (
            2,
            "Bookshop",
            "bookshop.test",
            2_499,
            "Paperback: The Glass Orchard",
        ),
        (5, "Cafe Verde", "cafeverde.test", 1_850, "Lunch"),
        (9, "Hardware Hub", "hardwarehub.test", 4_599, "USB-C dock"),
        (14, "Rail Co", "railco.test", 6_400, "Return ticket"),
        (
            19,
            "Outdoor Supply",
            "outdoorsupply.test",
            8_900,
            "Hiking boots",
        ),
        (
            24,
            "Bookshop",
            "bookshop.test",
            1_799,
            "Paperback: Night Ferry",
        ),
        (28, "Florist", "florist.test", 3_500, "Flowers for Carol"),
    ] {
        g.receipt(day, merchant, domain, cents, what, None, None);
    }
}

fn newsletters(g: &mut Gen, rng: &mut Rng) {
    // (sender, name, first day, every n days, subjects)
    let sources: [(&str, &str, i32, i32, &[&str]); 9] = [
        (
            "digest@techbrief.test",
            "TechBrief",
            1,
            1,
            &[
                "Today in tech: chips, clouds and courts",
                "Are you ready for the new phone season?",
                "The week's biggest funding rounds",
                "What does the new privacy law mean for you?",
            ],
        ),
        (
            "weekly@rustweekly.test",
            "Rust Weekly",
            7,
            7,
            &["This week in Rust", "Rust Weekly: async traits land"],
        ),
        (
            "news@citycouncil.test",
            "City Council",
            4,
            7,
            &[
                "Council news: road works on Elm Street",
                "Have your say on the new park?",
            ],
        ),
        (
            "hello@recipebox.test",
            "RecipeBox",
            1,
            2,
            &[
                "Five dinners in twenty minutes",
                "Can you guess this week's mystery ingredient?",
                "Autumn soups you'll make on repeat",
            ],
        ),
        (
            "deals@shopmart.test",
            "ShopMart",
            2,
            3,
            &[
                "48 hours only: 30% off home office",
                "Renew your ShopMart Plus membership by 2026-09-30 and save",
                "Did you forget something in your basket?",
            ],
        ),
        (
            "club@runclub.test",
            "Run Club",
            3,
            7,
            &[
                "Saturday long run: route inside",
                "Will we see you at the 10k?",
            ],
        ),
        (
            "events@museum.test",
            "City Museum",
            6,
            14,
            &["Late opening this Friday"],
        ),
        (
            "books@bookclub.test",
            "Book Club",
            5,
            7,
            &["October pick announced", "Can you join the author Q&A?"],
        ),
        (
            "show@podcast.test",
            "The Weekly Pod",
            2,
            5,
            &["New episode: inbox zero is a myth"],
        ),
    ];
    for (from, name, first, every, subjects) in sources {
        let mut day = first;
        let mut n = 0;
        while day <= 30 {
            // TechBrief skips weekends.
            if every == 1 && weekday(day) >= 5 {
                day += 1;
                continue;
            }
            let subject = subjects[(n + rng.below(subjects.len())) % subjects.len()];
            let body = format!(
                "{name}: {subject}. Read the full issue online. You are receiving this because you subscribed. Unsubscribe or manage preferences at any time."
            );
            let t = g.thread();
            let id = g.mail_full(day, &t, from, &[USER], subject, &body, true, false);
            g.distractor(&id, DistractorCategory::Newsletter, vec![Subj::Thread(t)]);
            n += 1;
            day += every;
        }
    }
}

fn notifications(g: &mut Gen, rng: &mut Rng) {
    // (sender, auto_submitted, count, subject and body templates)
    let sources: [(&str, bool, usize, &[(&str, &str)]); 6] = [
        (
            "no-reply@parcelpost.test",
            false,
            8,
            &[
                (
                    "Your parcel is on its way",
                    "Your parcel has shipped and will arrive in 2-3 days. Track it online.",
                ),
                (
                    "Delivered: your parcel",
                    "Your parcel was delivered to your front door.",
                ),
            ],
        ),
        (
            "noreply@buildbot.test",
            true,
            10,
            &[
                (
                    "Build failed: main #4512",
                    "The build on main failed at the test step. See the logs for details.",
                ),
                (
                    "Build fixed: main #4513",
                    "The build on main is green again.",
                ),
            ],
        ),
        (
            "alerts@firstbank.test",
            true,
            8,
            &[
                (
                    "Card payment of $42.10",
                    "A card payment was made at Cafe Verde. Not you? Call the number on your card.",
                ),
                (
                    "Your statement is ready",
                    "Your monthly statement is ready to view online.",
                ),
            ],
        ),
        (
            "notifications@docshare.test",
            false,
            8,
            &[
                (
                    "Priya commented on Q3 plan",
                    "Priya: can you take a look at section 2 when you get a chance?",
                ),
                (
                    "Marco shared a document with you",
                    "Marco shared 'Onboarding mockups v3'. Open it to view.",
                ),
                (
                    "Jonas mentioned you in Launch plan",
                    "Jonas: @Sam can you confirm this date?",
                ),
            ],
        ),
        (
            "calendar-noreply@example.com",
            true,
            6,
            &[
                (
                    "Reminder: Team sync at 10:00",
                    "This is a reminder for your upcoming event.",
                ),
                (
                    "Updated invitation: All hands",
                    "The event location has changed to Auditorium.",
                ),
            ],
        ),
        (
            "no-reply@accounts.test",
            true,
            6,
            &[
                (
                    "Did you just sign in from a new device?",
                    "We noticed a sign-in from a new device. If this was you, no action is needed.",
                ),
                (
                    "Reset your password",
                    "Did you request a password reset? If not, ignore this email.",
                ),
            ],
        ),
    ];
    for (from, auto, count, templates) in sources {
        for i in 0..count {
            let day = 1 + ((i * 30) / count) as i32 + rng.below(3) as i32;
            let day = day.min(30);
            let (subject, body) = templates[(i + rng.below(templates.len())) % templates.len()];
            let t = g.thread();
            let id = g.mail_full(day, &t, from, &[USER], subject, body, false, auto);
            g.distractor(&id, DistractorCategory::Notification, vec![Subj::Thread(t)]);
        }
    }
}

/// Bulk mail that looks personal. No headers flag it, and each sender writes about weekly, so an
/// engine can learn from the user's first dismissal.
fn bulk(g: &mut Gen) {
    let senders: [(&str, i32, &[(&str, &str)]); 4] = [
        (
            "carla@talentbridge.test",
            1,
            &[
                (
                    "Exciting opportunity",
                    "Hi Sam, are you open to a conversation about a Head of Platform role? Would Thursday work?",
                ),
                (
                    "Following up",
                    "Hi Sam, just checking whether you saw my last note. Could we find 15 minutes this week?",
                ),
            ],
        ),
        (
            "support@vendorly.test",
            2,
            &[
                (
                    "Ticket 88213 update",
                    "Hi Sam, did our last answer resolve your issue? Can you reply to let us know?",
                ),
                (
                    "How did we do?",
                    "Hi Sam, could you rate your recent support experience? It takes one minute.",
                ),
            ],
        ),
        (
            "events@meetup-hub.test",
            3,
            &[
                (
                    "Join us next week?",
                    "Hi Sam, will you join us for the platform engineering meetup? Please let us know if you can make it.",
                ),
                (
                    "Speakers wanted",
                    "Hi Sam, would you like to speak at a future meetup? Can you send us a topic?",
                ),
            ],
        ),
        (
            "success@acme-crm.test",
            4,
            &[
                (
                    "Quick check-in",
                    "Hi Sam, do you have 10 minutes for a check-in on your account this week?",
                ),
                (
                    "New dashboard",
                    "Hi Sam, have you tried the new dashboard yet? Could you share your feedback?",
                ),
            ],
        ),
    ];
    for (from, first, templates) in senders {
        for i in 0..6 {
            let day = first + 5 * i as i32;
            let (subject, body) = templates[i % templates.len()];
            let (id, t) = g.single(day, from, &[USER], subject, body);
            g.distractor(
                &id,
                DistractorCategory::BulkUnflagged,
                vec![Subj::Thread(t)],
            );
        }
    }
}

/// Renewal and charge notices for things the user never bought.
fn spam(g: &mut Gen) {
    let notices: [(i32, &str, &str, &str); 12] = [
        (
            2,
            "billing@secure-renewal-center.test",
            "Your Total Defense Plus subscription was renewed",
            "Your subscription has been renewed for $399.99. If you did not authorize this charge, call 555-0100 within 24 hours.",
        ),
        (
            4,
            "accounts@pc-guard-billing.test",
            "Renewal notice: PC Guard Ultimate",
            "Your PC Guard Ultimate plan renews today for $289.00. Did you authorize this renewal? Reply to cancel.",
        ),
        (
            6,
            "support@geek-help-desk.test",
            "Invoice #99812: annual protection plan",
            "Thank you for renewing your annual protection plan. Amount charged: $349.99.",
        ),
        (
            9,
            "billing@secure-renewal-center.test",
            "Final notice: auto-renewal",
            "Your auto-renewal will be charged tomorrow. Can you confirm your billing details?",
        ),
        (
            11,
            "service@cloud-storage-renew.test",
            "Storage plan renewal failed",
            "We could not renew your storage plan. Please update your payment details to avoid losing your files.",
        ),
        (
            13,
            "billing@antivirus-shield.test",
            "Subscription renewed: Shield 360",
            "Your Shield 360 subscription was renewed for $199.99. Questions? Call 555-0199.",
        ),
        (
            16,
            "accounts@pc-guard-billing.test",
            "Refund available for your renewal",
            "Would you like a refund for your recent renewal? Click to start the refund.",
        ),
        (
            18,
            "orders@streaming-plus-billing.test",
            "Your Streaming Plus membership renewed",
            "Your membership renewed for $119.88. If this was not you, contact us immediately.",
        ),
        (
            21,
            "support@geek-help-desk.test",
            "Payment confirmation",
            "Your annual protection plan renewal of $349.99 is confirmed.",
        ),
        (
            24,
            "billing@domain-renewal-office.test",
            "Domain renewal required",
            "Your domain is about to expire. Renew now for $89 to keep your website online.",
        ),
        (
            26,
            "service@cloud-storage-renew.test",
            "Last chance to renew",
            "Can you renew in the next 24 hours? Otherwise your files will be deleted.",
        ),
        (
            29,
            "billing@antivirus-shield.test",
            "Auto-renewal confirmation",
            "Your Shield 360 auto-renewal has been processed for $199.99.",
        ),
    ];
    for (day, from, subject, body) in notices {
        let (id, t) = g.single(day, from, &[USER], subject, body);
        let domain = from.split('@').nth(1).unwrap_or(from);
        g.distractor(
            &id,
            DistractorCategory::SpamRenewal,
            vec![Subj::Thread(t), Subj::Sub(domain.to_owned())],
        );
    }
}

/// Neutral traffic: FYI mail to and from the user, with no question and no promise.
fn fyi(g: &mut Gen, rng: &mut Rng) {
    let inbound = [
        "Notes from today's sync are in the shared folder.",
        "Heads up: the office is closed on Monday for maintenance.",
        "Thanks for the help yesterday, it worked.",
        "Sharing the article I mentioned. Worth a read.",
        "The release went out this morning. All green.",
        "Photos from the weekend are in the album.",
        "FYI, I'll be out Thursday afternoon.",
        "Great presentation today. The team loved it.",
        "The new coffee machine is finally here.",
        "Minutes from the vendor call are attached.",
    ];
    let subjects = [
        "Sync notes",
        "Office closed Monday",
        "Thanks!",
        "Interesting read",
        "Release done",
        "Weekend photos",
        "Out Thursday",
        "Nice work",
        "Coffee",
        "Vendor call minutes",
    ];
    let senders: Vec<&str> = COLLEAGUES
        .iter()
        .chain(CONTACTS.iter())
        .copied()
        .chain([ALEX])
        .collect();
    for i in 0..40 {
        let day = 1 + i * 30 / 40;
        let from = senders[rng.below(senders.len())];
        let k = rng.below(inbound.len());
        g.single(day, from, &[USER], subjects[k], inbound[k]);
    }
    let outbound = [
        (
            "Slides from today",
            "Slides from today's session, for reference.",
        ),
        ("Thanks", "Thanks for your time today, really useful."),
        ("Article", "Thought you'd like this one."),
        (
            "Recap",
            "Quick recap: we agreed to ship the beta next month.",
        ),
    ];
    for i in 0..8 {
        let day = 2 + i * 28 / 8;
        let to = senders[rng.below(senders.len())];
        let (s, b) = outbound[rng.below(outbound.len())];
        g.single(day, USER, &[to], s, b);
    }
}

fn notes(g: &mut Gen) {
    for (day, title, what, due) in NOTE_COMMITMENTS {
        let id = g.note(
            day,
            title,
            &format!(
                "- [ ] {what} (due {})\n- [ ] Water the plants",
                iso_date(due)
            ),
        );
        g.gold(
            CardKind::CommitmentDue,
            Subj::Commit(id.clone()),
            (due - 2, due),
            &id,
            &format!("Note todo due {}", pretty_date(due)),
        );
    }
    let done = g.note(
        1,
        "Work todos",
        &format!(
            "- [x] Submit the expense report (due {}), done",
            iso_date(5)
        ),
    );
    g.distractor(
        &done,
        DistractorCategory::CompletedCommitment,
        vec![Subj::Commit(done.clone())],
    );
    for (day, title, body) in [
        (
            1,
            "Ideas",
            "Search should rank recent threads higher. Try a weekly digest.",
        ),
        (4, "Groceries", "Oats, lemons, coffee, rice."),
        (
            7,
            "1:1 notes",
            "Talked about the hiring plan and the offsite.",
        ),
        (
            10,
            "Reading list",
            "The Glass Orchard; Night Ferry; a book on negotiation.",
        ),
        (
            12,
            "Northwind QBR notes",
            "Pilot went well; they want a 12-month term.",
        ),
        (15, "Gift ideas", "Mom: scarf or a cookbook."),
        (
            18,
            "Ideas",
            "Card inbox should explain why each card is shown.",
        ),
        (21, "Trail plan", "Ridge trail, 14 km, start at 8."),
        (
            25,
            "Retro",
            "What went well: launch prep. To improve: fewer meetings.",
        ),
        (29, "Groceries", "Apples, pasta, tea."),
    ] {
        g.note(day, title, body);
    }
}

fn finish(g: Gen) -> Fixture {
    let Gen {
        mut items,
        gold,
        distractors,
        ..
    } = g;
    // Stable order: by day, then by kind (events and receipts before mail), then build order.
    let rank = |i: &Item| match i {
        Item::Receipt(_) => 0,
        Item::Event(_) => 1,
        Item::EventCancelled(_) => 2,
        Item::Note(_) => 3,
        Item::Mail(_) => 4,
    };
    let seq = |i: &Item| i.id()[1..].parse::<usize>().unwrap_or(0);
    items.sort_by_key(|i| (i.day(), rank(i), seq(i)));

    let mut ids: HashMap<String, String> = HashMap::new();
    let mut threads: HashMap<String, String> = HashMap::new();
    let mut counts = [0usize; 5];
    for item in &mut items {
        let (prefix, n, width) = match item {
            Item::Mail(_) => ("m", &mut counts[0], 4),
            Item::Event(_) => ("e", &mut counts[1], 3),
            Item::EventCancelled(_) => ("x", &mut counts[2], 3),
            Item::Note(_) => ("n", &mut counts[3], 3),
            Item::Receipt(_) => ("r", &mut counts[4], 3),
        };
        *n += 1;
        let new = format!("{prefix}-{:0width$}", *n);
        ids.insert(item.id().to_owned(), new.clone());
        match item {
            Item::Mail(m) => {
                m.id = new;
                let next = threads.len() + 1;
                m.thread = threads
                    .entry(m.thread.clone())
                    .or_insert_with(|| format!("t-{next:03}"))
                    .clone();
            }
            Item::Event(e) => e.id = new,
            Item::EventCancelled(c) => c.id = new,
            Item::Note(n) => n.id = new,
            Item::Receipt(r) => r.id = new,
        }
    }
    for item in &mut items {
        if let Item::EventCancelled(c) = item {
            c.event = ids[&c.event].clone();
        }
    }
    let subject = |s: &Subj| match s {
        Subj::Thread(t) => thread_subject(&threads[t]),
        Subj::Event(e) => event_subject(&ids[e]),
        Subj::Commit(i) => commit_subject(&ids[i]),
        Subj::Sub(d) => sub_subject(d),
    };
    let mut cards: Vec<GoldCard> = gold
        .iter()
        .map(|d| GoldCard {
            id: String::new(),
            kind: d.kind,
            subject: subject(&d.subject),
            first_day: d.first_day,
            last_day: d.last_day,
            source: ids[&d.source].clone(),
            why: d.why.clone(),
        })
        .collect();
    cards.sort_by(|a, b| (a.first_day, a.kind, &a.subject).cmp(&(b.first_day, b.kind, &b.subject)));
    for (i, c) in cards.iter_mut().enumerate() {
        c.id = format!("g-{:03}", i + 1);
    }
    let mut distractors: Vec<Distractor> = distractors
        .iter()
        .map(|(item, category, subjects)| Distractor {
            item: ids[item].clone(),
            category: *category,
            subjects: subjects.iter().map(subject).collect(),
        })
        .collect();
    distractors.sort_by(|a, b| a.item.cmp(&b.item));

    Fixture {
        world: World {
            user: Person {
                name: "Sam Rivera".to_owned(),
                email: USER.to_owned(),
            },
            start_date: iso_date(1),
            days: super::world::DAYS,
            items,
        },
        gold: Gold { cards, distractors },
    }
}
