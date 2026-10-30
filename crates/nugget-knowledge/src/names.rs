//! People's names: given-name equivalence (nicknames and diminutives), short forms of a full
//! name, and the name hints in an email address's local part. Pure and deterministic; used by
//! entity resolution ([`crate::resolve`]) to decide whether "Maggie", "M. Thistlewood",
//! "Maggie T" or `mthistlewood@` *could* be Margaret Thistlewood. Whether they *are* is decided
//! there, from corroborating evidence: a name match alone never merges two people.
//!
//! The diminutive table below was written for this crate from general knowledge of English
//! naming customs. It is deliberately small (common forms only) and conservative: a nickname
//! listed under several names ("Sam": Samuel, Samantha) matches each of them, and names that
//! merely share letters ("Mark", "Margaret") are unrelated.

use nugget_graph::normalize_name;

/// Canonical given names and their pure diminutives and nicknames (lower case): forms that are
/// rarely a given name of their own. See [`STANDALONE`] for the ones that are.
const DIMINUTIVES: &[(&str, &[&str])] = &[
    ("abigail", &["abby", "abbie"]),
    ("abraham", &["abe", "bram"]),
    ("adelaide", &["addie"]),
    ("agnes", &["aggie"]),
    ("albert", &["al", "bert", "bertie"]),
    ("alexander", &["alex", "alec", "al", "xander", "lex"]),
    ("alexandra", &["alex", "lexi", "alexa"]),
    ("alfred", &["alf", "alfie", "fred", "freddie", "al"]),
    ("alice", &["ally", "allie"]),
    ("alison", &["ali", "allie"]),
    ("amanda", &["mandy", "manda"]),
    ("andrew", &["andy"]),
    ("angela", &["angie"]),
    ("anne", &["annie"]),
    ("anna", &["annie"]),
    ("anthony", &["tony", "ant"]),
    ("antonia", &["toni"]),
    ("arthur", &["art", "artie"]),
    ("barbara", &["barb", "babs", "barbie"]),
    ("bartholomew", &["bart", "barty"]),
    ("beatrice", &["bea", "bee", "trixie"]),
    ("benjamin", &["ben", "benny", "benji"]),
    ("bernard", &["bernie"]),
    (
        "catherine",
        &["cathy", "cath", "kate", "katie", "kit", "cat"],
    ),
    (
        "katherine",
        &["kathy", "kath", "kate", "katie", "kit", "kat"],
    ),
    ("charles", &["charlie", "chuck", "chas", "chaz"]),
    ("charlotte", &["charlie", "lotte"]),
    ("christian", &["chris"]),
    ("christina", &["chris", "chrissy"]),
    ("christine", &["chris", "chrissy"]),
    ("christopher", &["chris", "kit", "topher"]),
    ("clement", &["clem"]),
    ("clementine", &["clem", "clemmie"]),
    ("cornelius", &["corny"]),
    ("cynthia", &["cindy"]),
    ("daniel", &["dan", "danny"]),
    ("danielle", &["dani"]),
    ("david", &["dave", "davy", "davey"]),
    ("deborah", &["deb", "debbie", "debby"]),
    ("dennis", &["denny"]),
    ("dominic", &["dom"]),
    ("donald", &["don", "donnie"]),
    ("dorothy", &["dot", "dottie", "dolly"]),
    ("douglas", &["doug"]),
    ("edmund", &["ed", "eddie", "ned"]),
    ("edward", &["ed", "eddie", "eddy", "ned", "ted", "teddy"]),
    ("edwin", &["ed", "eddie"]),
    ("eleanor", &["ellie", "nellie"]),
    (
        "elizabeth",
        &["liz", "lizzie", "beth", "betty", "bess", "betsy", "libby"],
    ),
    ("emily", &["em", "emmy"]),
    ("emma", &["em", "emmy"]),
    ("evelyn", &["evie"]),
    ("florence", &["flo", "florrie"]),
    ("frances", &["fran", "frankie", "fanny"]),
    ("francis", &["frank", "frankie", "fran"]),
    ("frederick", &["fred", "freddie", "freddy", "rick"]),
    ("gabriel", &["gabe"]),
    ("gabrielle", &["gabby", "gabi"]),
    ("geoffrey", &["geoff"]),
    ("gerald", &["gerry", "jerry"]),
    ("gregory", &["greg"]),
    ("gwendolyn", &["gwen"]),
    ("harold", &["hal"]),
    ("harriet", &["hattie"]),
    ("henry", &["hank", "hal"]),
    ("herbert", &["herb", "bert"]),
    ("isabella", &["izzy", "isa"]),
    ("isabel", &["bel", "izzy", "isa"]),
    ("jacob", &["jake"]),
    ("james", &["jim", "jimmy", "jamie", "jem"]),
    ("jane", &["janie", "jenny"]),
    ("jennifer", &["jen", "jenny", "jenn"]),
    ("jessica", &["jess", "jessie"]),
    ("joanna", &["jo", "joanie"]),
    ("joanne", &["jo", "joanie"]),
    ("john", &["johnny", "jon"]),
    ("jonathan", &["jon", "jonny"]),
    ("joseph", &["joe", "joey", "jos"]),
    ("josephine", &["jo", "josie"]),
    ("joshua", &["josh"]),
    ("judith", &["judy", "jude"]),
    ("julia", &["jules", "julie"]),
    ("katrina", &["kat"]),
    ("kenneth", &["ken", "kenny"]),
    ("lawrence", &["larry", "laurie"]),
    ("leonard", &["len", "lenny"]),
    ("louisa", &["lou", "lulu"]),
    ("louise", &["lou", "lulu"]),
    ("lucinda", &["lucy", "cindy"]),
    ("madeline", &["maddie", "maddy"]),
    (
        "margaret",
        &["maggie", "meg", "peggy", "madge", "marge", "margie"],
    ),
    ("matilda", &["tilly", "tilda", "mattie"]),
    ("matthew", &["matt", "matty"]),
    ("michael", &["mike", "mick", "mickey", "mikey"]),
    ("nathaniel", &["nat", "nate"]),
    ("nicholas", &["nick", "nicky", "nico"]),
    ("nicola", &["nicky", "nikki"]),
    ("oliver", &["ollie", "olly"]),
    ("olivia", &["liv", "livvy", "ollie"]),
    ("patricia", &["pat", "patty", "trish", "tricia"]),
    ("patrick", &["pat", "paddy"]),
    ("penelope", &["penny", "pen"]),
    ("peter", &["pete"]),
    ("philip", &["phil", "pip"]),
    ("phillip", &["phil"]),
    ("rebecca", &["becky", "becca", "bex"]),
    ("richard", &["rich", "rick", "ricky", "dick", "richie"]),
    ("robert", &["rob", "robbie", "bob", "bobby", "bert"]),
    ("ronald", &["ron", "ronnie"]),
    ("rosalind", &["ros", "roz", "rosie"]),
    ("rosemary", &["rosie"]),
    ("samantha", &["sam", "sammy"]),
    ("samuel", &["sam", "sammy"]),
    ("sarah", &["sally"]),
    ("sebastian", &["seb", "bastian"]),
    ("stephanie", &["steph", "stevie"]),
    ("stephen", &["steve", "stevie"]),
    ("steven", &["steve", "stevie"]),
    ("susan", &["sue", "susie", "suzy"]),
    ("suzanne", &["sue", "suzy"]),
    ("theodore", &["theo", "ted", "teddy"]),
    ("theresa", &["tess", "terri"]),
    ("thomas", &["tom", "tommy"]),
    ("timothy", &["tim", "timmy"]),
    ("tobias", &["toby"]),
    ("valentina", &["val"]),
    ("valerie", &["val"]),
    ("victoria", &["vicky", "tori", "vic"]),
    ("vincent", &["vince", "vinny"]),
    ("virginia", &["ginny", "ginger"]),
    ("walter", &["walt", "wally"]),
    ("william", &["will", "bill", "billy", "willy"]),
    ("winifred", &["winnie"]),
    ("zachary", &["zach", "zack", "zac"]),
];

/// Names that are diminutives of a longer name *and* common given names in their own right
/// ("Liam" is a name, not only short for William; "Nathan", "Harry", "Ada", "Rose"). They never
/// count as equivalent to a different full given name: "Liam Murphy" and "William Murphy" may be
/// two people. They are kept out of the table above; the list documents why and guards it.
pub(crate) const STANDALONE: &[&str] = &[
    "ada", "bella", "daisy", "dora", "drew", "eliza", "elle", "eve", "flora", "freda", "gail",
    "gene", "greta", "harry", "jack", "kay", "leo", "liam", "lisa", "lottie", "maisie", "millie",
    "mina", "molly", "nan", "nancy", "nathan", "nell", "nora", "posy", "rose", "sadie", "sandra",
    "sandy", "tessa", "tina", "trina", "wendy", "willa",
];

/// The canonical names a given name stands for: itself, plus every canonical name that lists
/// it as a diminutive (none for a [`STANDALONE`] name). Lower-case input.
fn keys(given: &str) -> Vec<&str> {
    if STANDALONE.contains(&given) {
        return vec![given];
    }
    let mut out: Vec<&str> = DIMINUTIVES
        .iter()
        .filter(|(_, nicks)| nicks.contains(&given))
        .map(|(c, _)| *c)
        .collect();
    out.push(given);
    out
}

/// Whether two given names name the same person's given name: equal, or one is a listed
/// diminutive of the other, or both are diminutives of one name. Symmetric; case-insensitive.
/// `Maggie` ~ `Margaret`, `Bob` ~ `Robert` ~ `Rob`; not `Maggie` ~ `Mark`.
pub(crate) fn given_names_equivalent(a: &str, b: &str) -> bool {
    let (a, b) = (clean_token(a), clean_token(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let ka = keys(&a);
    keys(&b).iter().any(|k| ka.contains(k))
}

/// Every given name equivalent to `given` (itself included), lower case, sorted.
pub(crate) fn given_name_variants(given: &str) -> Vec<String> {
    let g = clean_token(given);
    if g.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = vec![g.clone()];
    for k in keys(&g) {
        out.push(k.to_owned());
        if let Some((_, nicks)) = DIMINUTIVES.iter().find(|(c, _)| *c == k) {
            out.extend(
                nicks
                    .iter()
                    .filter(|n| !STANDALONE.contains(n))
                    .map(|n| (*n).to_owned()),
            );
        }
    }
    out.sort();
    out.dedup();
    out
}

/// One part of a name: written out, or an initial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Part {
    Full(String),
    Initial(char),
}

impl Part {
    fn first(&self) -> Option<char> {
        match self {
            Part::Full(s) => s.chars().next(),
            Part::Initial(c) => Some(*c),
        }
    }

    fn is_full(&self) -> bool {
        matches!(self, Part::Full(_))
    }
}

/// A person's name taken apart: the given name and the surname (middle names are ignored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersonName {
    pub given: Part,
    pub surname: Option<Part>,
}

const HONORIFICS: &[&str] = &[
    "dr",
    "mr",
    "mrs",
    "ms",
    "miss",
    "mx",
    "prof",
    "professor",
    "sir",
    "dame",
    "rev",
];
const SUFFIXES: &[&str] = &["jr", "sr", "ii", "iii", "iv", "phd", "md", "esq"];
const PARTICLES: &[&str] = &[
    "van", "von", "de", "der", "den", "da", "di", "del", "della", "du", "la", "le", "st",
];

fn clean_token(t: &str) -> String {
    t.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

impl PersonName {
    /// Parse a display name ("Margaret Thistlewood", "M. Thistlewood", "Maggie T", "Dr. Nadia
    /// Kerrigan-Holt", "Maggie"). `None` for anything that does not look like a name (an
    /// address, digits, nothing left).
    pub fn parse(name: &str) -> Option<Self> {
        if name.contains('@') {
            return None;
        }
        let norm = normalize_name(name);
        let mut toks: Vec<String> = norm
            .split([' ', ','])
            .map(clean_token)
            .filter(|t| !t.is_empty())
            .collect();
        if toks
            .iter()
            .any(|t| t.chars().any(|c| c.is_ascii_digit()) || !t.chars().any(char::is_alphabetic))
        {
            return None;
        }
        let mut titled = false;
        while toks.len() > 1 && HONORIFICS.contains(&toks[0].as_str()) {
            toks.remove(0);
            titled = true;
        }
        if titled && toks.len() == 1 {
            // "Ms. Jordan", "Dr. Kerrigan-Holt": a surname, not a given name.
            return None;
        }
        while toks.len() > 1 && toks.last().is_some_and(|t| SUFFIXES.contains(&t.as_str())) {
            toks.pop();
        }
        let part = |t: &str| {
            let mut cs = t.chars();
            match (cs.next(), cs.next()) {
                (Some(c), None) => Part::Initial(c),
                _ => Part::Full(t.to_owned()),
            }
        };
        let given = part(toks.first()?);
        if toks.len() == 1 {
            return Some(Self {
                given,
                surname: None,
            });
        }
        // The surname: the last word, with the particles right before it.
        let mut start = toks.len() - 1;
        while start > 1 && PARTICLES.contains(&toks[start - 1].as_str()) {
            start -= 1;
        }
        let surname = if start + 1 == toks.len() {
            part(&toks[start])
        } else {
            Part::Full(toks[start..].join(" "))
        };
        Some(Self {
            given,
            surname: Some(surname),
        })
    }

    /// A written-out given name and surname: a name other forms can be short for.
    pub fn is_complete(&self) -> bool {
        self.given.is_full() && self.surname.as_ref().is_some_and(Part::is_full)
    }

    pub fn given_full(&self) -> Option<&str> {
        match &self.given {
            Part::Full(s) => Some(s),
            Part::Initial(_) => None,
        }
    }

    pub fn surname_full(&self) -> Option<&str> {
        match &self.surname {
            Some(Part::Full(s)) => Some(s),
            _ => None,
        }
    }
}

/// Whether a name is a short form: one word, or with an initial for a part ("M. Thistlewood",
/// "Maggie T"). `false` for a written-out given name and surname, and for non-names.
pub(crate) fn is_short_form(name: &str) -> bool {
    PersonName::parse(name).is_some_and(|p| !p.is_complete())
}

/// Why a name may be short for another (see [`compatible`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NameMatch {
    /// `first_name`, `nickname`, `initial`, `same_name`.
    pub reasons: Vec<&'static str>,
}

/// Whether `short` may be another way of writing `full`. `full` must have a written-out given
/// name and a surname (written out, or at least an initial), and `short` must say no more than
/// it, part by part:
/// - the given name: the same, an equivalent nickname (`nickname`), or its initial (`initial`);
/// - the surname: the same, its initial (`initial`), or left out (first name or nickname
///   alone: `first_name` / `nickname`).
///
/// "Maggie", "Margaret", "M. Thistlewood", "Maggie T", "Maggie Thistlewood" are all compatible
/// with "Margaret Thistlewood"; "Mark", "Maggie Smith", "M. T" are not. Two equal names are not
/// short for each other (`None`), except when only middle names differ (`same_name`).
pub(crate) fn compatible(short: &str, full: &str) -> Option<NameMatch> {
    if normalize_name(short) == normalize_name(full) {
        return None;
    }
    let s = PersonName::parse(short)?;
    let f = PersonName::parse(full)?;
    let fg = f.given_full()?;
    let fs = f.surname.as_ref()?;
    let mut reasons: Vec<&'static str> = Vec::new();
    // Given name.
    match &s.given {
        Part::Full(g) if g == fg => {}
        Part::Full(g) if given_names_equivalent(g, fg) => reasons.push("nickname"),
        Part::Full(_) => return None,
        Part::Initial(c) if fg.starts_with(*c) => reasons.push("initial"),
        Part::Initial(_) => return None,
    }
    // Surname.
    match (&s.surname, fs) {
        (None, _) => {
            if !s.given.is_full() {
                return None;
            }
            if reasons.is_empty() {
                reasons.push("first_name");
            }
        }
        (Some(Part::Full(a)), Part::Full(b)) => {
            if a != b {
                return None;
            }
        }
        (Some(Part::Initial(a)), sur) => {
            if Some(*a) != sur.first() || !s.given.is_full() {
                return None;
            }
            if sur.is_full() && !reasons.contains(&"initial") {
                reasons.push("initial");
            }
        }
        // The short side writes out a surname the full side only has an initial of.
        (Some(Part::Full(_)), Part::Initial(_)) => return None,
    }
    if reasons.is_empty() {
        // Same given name and surname: only middle names (or honorifics) differ.
        reasons.push("same_name");
    }
    Some(NameMatch { reasons })
}

/// Local parts that name a role or a machine, not a person.
const ROLE_LOCALS: &[&str] = &[
    "info",
    "admin",
    "hello",
    "hi",
    "team",
    "support",
    "contact",
    "office",
    "sales",
    "billing",
    "accounts",
    "enquiries",
    "inquiries",
    "mail",
    "post",
    "news",
    "newsletter",
    "noreply",
    "reply",
    "help",
    "jobs",
    "careers",
    "press",
    "reception",
    "bookings",
    "booking",
    "events",
    "winner",
    "service",
    "orders",
    "webmaster",
    "hr",
    "it",
    "finance",
    "members",
    "membership",
];

/// The words of an email address's local part (`Maggie.T+news@x` gives `maggie`, `t`), lower
/// case, digits dropped. `None` for role and machine addresses (`info@`, `noreply@`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalPart {
    pub words: Vec<String>,
}

/// How much of a name a local part spells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalMatch {
    /// The given name (or a nickname) alone: `sam@`.
    Given,
    /// The given name and the surname, whole or as initials: `maggie.t@`, `mthistlewood@`,
    /// `margaret.thistlewood@`, `thistlewood.m@`.
    Full,
}

impl LocalPart {
    /// Parse an address or a bare local part.
    pub fn parse(addr: &str) -> Option<Self> {
        let local = addr.rsplit_once('@').map_or(addr, |(l, _)| l);
        let local = local.split('+').next().unwrap_or_default().to_lowercase();
        let words: Vec<String> = local
            .split(['.', '_', '-'])
            .map(|w| {
                w.chars()
                    .filter(char::is_ascii_alphabetic)
                    .collect::<String>()
            })
            .filter(|w| !w.is_empty())
            .collect();
        if words.is_empty()
            || words.len() > 4
            || words.iter().any(|w| ROLE_LOCALS.contains(&w.as_str()))
            || ROLE_LOCALS.contains(&words.concat().as_str())
        {
            return None;
        }
        Some(Self { words })
    }

    /// The name hints the local part gives on its own: for `first.last` forms, a given name
    /// and a surname (either may be an initial); for one word, a given name (it may also be a
    /// run-together name, which only [`LocalPart::matches`] can tell).
    pub fn hints(&self) -> Option<PersonName> {
        let part = |w: &str| {
            let mut cs = w.chars();
            match (cs.next(), cs.next()) {
                (Some(c), None) => Part::Initial(c),
                _ => Part::Full(w.to_owned()),
            }
        };
        match &self.words[..] {
            [g] if g.chars().count() >= 2 => Some(PersonName {
                given: Part::Full(g.clone()),
                surname: None,
            }),
            [g, s] => Some(PersonName {
                given: part(g),
                surname: Some(part(s)),
            }),
            _ => None,
        }
    }

    /// How much of `full` (a name with a written-out given name) the local part spells.
    pub fn matches(&self, full: &str) -> Option<LocalMatch> {
        let f = PersonName::parse(full)?;
        let given = f.given_full()?;
        let gi = given.chars().next()?;
        let surname: Option<String> = f.surname_full().map(|s| {
            s.chars()
                .filter(char::is_ascii_alphabetic)
                .collect::<String>()
        });
        let si = f.surname.as_ref().and_then(Part::first);
        let is_given = |w: &str| w.chars().count() >= 2 && given_names_equivalent(w, given);
        let is_gi = |w: &str| w.chars().count() == 1 && w.starts_with(gi);
        let is_sur = |w: &str| surname.as_deref().is_some_and(|s| s.len() >= 2 && s == w);
        let is_si = |w: &str| w.chars().count() == 1 && si.is_some_and(|c| w.starts_with(c));
        match &self.words[..] {
            [a, b] => {
                let given_then_sur =
                    (is_given(a) && (is_sur(b) || is_si(b))) || (is_gi(a) && is_sur(b));
                let sur_then_given = is_sur(a) && (is_given(b) || is_gi(b));
                (given_then_sur || sur_then_given).then_some(LocalMatch::Full)
            }
            [a, _, c] => ((is_given(a) || is_gi(a)) && is_sur(c)).then_some(LocalMatch::Full),
            [w] => {
                if is_given(w) {
                    return Some(LocalMatch::Given);
                }
                let sur = surname.as_deref().filter(|s| s.len() >= 2)?;
                let gi = gi.to_string();
                let joined = [
                    format!("{given}{sur}"),
                    format!("{gi}{sur}"),
                    format!("{sur}{given}"),
                    format!("{sur}{gi}"),
                ];
                if joined.iter().any(|j| j == w) {
                    return Some(LocalMatch::Full);
                }
                // A nickname run together with the surname or its initial (`maggiet`).
                let nick = w
                    .strip_suffix(sur)
                    .or_else(|| si.and_then(|c| w.strip_suffix(c)))
                    .filter(|g| is_given(g));
                nick.map(|_| LocalMatch::Full)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nicknames_are_equivalent_both_ways() {
        for (a, b) in [
            ("Maggie", "Margaret"),
            ("Margaret", "Maggie"),
            ("Bob", "Robert"),
            ("Rob", "Robert"),
            ("Bob", "Rob"),
            ("Peggy", "Maggie"),
            ("bea", "Beatrice"),
            ("Theo", "Theodore"),
            ("Clem", "Clementine"),
            ("Bart", "Bartholomew"),
            ("Sam", "Samantha"),
            ("Sam", "Samuel"),
            ("Liz", "Elizabeth"),
            ("priya", "Priya"),
        ] {
            assert!(given_names_equivalent(a, b), "{a} ~ {b}");
            assert!(given_names_equivalent(b, a), "{b} ~ {a}");
        }
        for (a, b) in [
            ("Maggie", "Mark"),
            ("Margaret", "Mark"),
            ("Sam", "Sara"),
            ("Robert", "Roberta"),
            ("Samuel", "Samantha"),
            ("Bob", "Bill"),
            ("Mike", "Mark"),
            ("", "Mark"),
            ("Priya", "Pria"),
        ] {
            assert!(!given_names_equivalent(a, b), "{a} !~ {b}");
            assert!(!given_names_equivalent(b, a), "{b} !~ {a}");
        }
        let v = given_name_variants("Maggie");
        assert!(v.contains(&"margaret".to_string()) && v.contains(&"peggy".to_string()));
        assert!(!v.contains(&"mark".to_string()));
        assert_eq!(given_name_variants("Wenna"), vec!["wenna".to_string()]);
    }

    #[test]
    fn names_parse() {
        let p = PersonName::parse("Dr. Nadia Kerrigan-Holt").unwrap();
        assert_eq!(p.given, Part::Full("nadia".into()));
        assert_eq!(p.surname, Some(Part::Full("kerrigan-holt".into())));
        assert!(p.is_complete());
        let p = PersonName::parse("M. Thistlewood").unwrap();
        assert_eq!(p.given, Part::Initial('m'));
        assert!(!p.is_complete());
        let p = PersonName::parse("Maggie T.").unwrap();
        assert_eq!(p.surname, Some(Part::Initial('t')));
        let p = PersonName::parse("Margaret A. Thistlewood").unwrap();
        assert_eq!(p.surname, Some(Part::Full("thistlewood".into())));
        let p = PersonName::parse("Sophie van der Berg").unwrap();
        assert_eq!(p.surname, Some(Part::Full("van der berg".into())));
        let p = PersonName::parse("Robert Chen Jr.").unwrap();
        assert_eq!(p.surname, Some(Part::Full("chen".into())));
        assert!(PersonName::parse("maggie.t@mail.test").is_none());
        assert!(PersonName::parse("Room 101").is_none());
        assert!(PersonName::parse("  ").is_none());
        assert!(is_short_form("Maggie") && is_short_form("M. Thistlewood"));
        assert!(is_short_form("Maggie T") && !is_short_form("Maggie Thistlewood"));
        assert!(!is_short_form("x@y.test"));
    }

    #[test]
    fn short_forms_of_a_full_name() {
        let full = "Margaret Thistlewood";
        let ok = |s: &str, f: &str| compatible(s, f).map(|m| m.reasons);
        assert_eq!(ok("Margaret", full), Some(vec!["first_name"]));
        assert_eq!(ok("Maggie", full), Some(vec!["nickname"]));
        assert_eq!(ok("M. Thistlewood", full), Some(vec!["initial"]));
        assert_eq!(ok("M Thistlewood", full), Some(vec!["initial"]));
        assert_eq!(ok("Maggie T", full), Some(vec!["nickname", "initial"]));
        assert_eq!(ok("Margaret T.", full), Some(vec!["initial"]));
        assert_eq!(ok("Maggie Thistlewood", full), Some(vec!["nickname"]));
        assert_eq!(ok("Bob Chen", "Robert Chen"), Some(vec!["nickname"]));
        assert_eq!(ok("Bob", "Robert Chen"), Some(vec!["nickname"]));
        assert_eq!(ok("Robert", "Bob Chen"), Some(vec!["nickname"]));
        assert_eq!(
            ok("Margaret A. Thistlewood", full),
            Some(vec!["same_name"]),
            "middle names only"
        );
        // A short header name is a full enough target for a first name.
        assert_eq!(ok("Maggie", "Maggie T"), Some(vec!["first_name"]));
        assert_eq!(ok("Sam", "Dr. Sam Okafor"), Some(vec!["first_name"]));
        // Negatives.
        for (s, f) in [
            ("Mark", full),
            ("Maggie Smith", full),
            ("M. Smith", full),
            ("K. Thistlewood", full),
            ("Maggie K", full),
            ("M. T", full),
            ("M.", full),
            (full, full),
            ("Margaret Thistlewood", "Maggie T"), // says more than the target
            ("Maggie", "Maggie"),
            ("Maggie", "M. Thistlewood"), // the target has no written-out given name
            ("Sam", "Samira Okafor"),
            ("Thistlewood", full), // a surname alone is not handled
            ("maggie.t@mail.test", full),
            ("Robert", "Roberta Chen"),
        ] {
            assert_eq!(compatible(s, f), None, "{s} vs {f}");
        }
    }

    #[test]
    fn email_local_parts() {
        let full = "Margaret Thistlewood";
        let m = |a: &str| LocalPart::parse(a).and_then(|l| l.matches(full));
        assert_eq!(m("maggie.t@mail.test"), Some(LocalMatch::Full));
        assert_eq!(m("mthistlewood@cindergate.test"), Some(LocalMatch::Full));
        assert_eq!(m("margaret.thistlewood@x.test"), Some(LocalMatch::Full));
        assert_eq!(m("m.thistlewood@x.test"), Some(LocalMatch::Full));
        assert_eq!(m("thistlewood.m@x.test"), Some(LocalMatch::Full));
        assert_eq!(
            m("Margaret_Thistlewood+news@x.test"),
            Some(LocalMatch::Full)
        );
        assert_eq!(m("margaretthistlewood@x.test"), Some(LocalMatch::Full));
        assert_eq!(m("maggiet@x.test"), Some(LocalMatch::Full));
        assert_eq!(m("margaret.a.thistlewood@x.test"), Some(LocalMatch::Full));
        assert_eq!(m("maggie@x.test"), Some(LocalMatch::Given));
        assert_eq!(m("maggie42@x.test"), Some(LocalMatch::Given));
        for a in [
            "mark.t@x.test",
            "maggie.k@x.test",
            "mthistle@x.test",
            "thistlewood@x.test",
            "t.maggie@x.test",
            "info@x.test",
            "noreply@x.test",
            "mt@x.test",
            "1234@x.test",
        ] {
            assert_eq!(m(a), None, "{a}");
        }
        assert_eq!(
            LocalPart::parse("n.kerrigan@saltmarsh-clinic.test")
                .unwrap()
                .matches("Nadia Kerrigan-Holt"),
            None,
            "a different surname spelling is not guessed"
        );
        let h = LocalPart::parse("maggie.t@mail.test")
            .unwrap()
            .hints()
            .unwrap();
        assert_eq!(h.given, Part::Full("maggie".into()));
        assert_eq!(h.surname, Some(Part::Initial('t')));
        let h = LocalPart::parse("sam@marrowbank-rc.test")
            .unwrap()
            .hints()
            .unwrap();
        assert_eq!((h.given, h.surname), (Part::Full("sam".into()), None));
        assert!(LocalPart::parse("team@x.test").is_none());
    }

    #[test]
    fn standalone_names_and_wrong_mappings_never_match_another_name() {
        for (a, b) in [
            ("Liam", "William"),
            ("Nathan", "Jonathan"),
            ("Nathan", "Nathaniel"),
            ("Jack", "John"),
            ("Harry", "Henry"),
            ("Ada", "Adelaide"),
            ("Lisa", "Elizabeth"),
            ("Rose", "Rosemary"),
            ("Molly", "Margaret"),
            ("Daisy", "Margaret"),
            ("Neil", "Cornelius"),
            ("Connie", "Cornelius"),
            ("Nessa", "Agnes"),
        ] {
            assert!(!given_names_equivalent(a, b), "{a} !~ {b}");
            assert!(!given_names_equivalent(b, a), "{b} !~ {a}");
        }
        for n in STANDALONE {
            assert_eq!(given_name_variants(n), vec![(*n).to_string()], "{n}");
            assert!(
                DIMINUTIVES
                    .iter()
                    .all(|(c, nicks)| c != n && !nicks.contains(n)),
                "{n} is in the diminutive table"
            );
        }
        assert!(!given_name_variants("William").contains(&"liam".to_string()));
        assert_eq!(compatible("Liam", "William Murphy"), None);
        assert_eq!(compatible("Nathan Lee", "Jonathan Lee"), None);
    }

    #[test]
    fn a_title_before_one_word_is_a_surname() {
        assert!(PersonName::parse("Ms. Jordan").is_none());
        assert!(PersonName::parse("Dr Kerrigan-Holt").is_none());
        assert!(!is_short_form("Ms. Jordan"));
        assert_eq!(compatible("Ms. Jordan", "Jordan Lee"), None);
        assert_eq!(compatible("Mr Sam", "Sam Okafor"), None);
        // A title before a full name is dropped as before.
        assert!(PersonName::parse("Ms. Jordan Lee").is_some_and(|p| p.is_complete()));
    }
}
