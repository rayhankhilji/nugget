use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nugget_core::ids::{EntityId, ItemKey, ItemRef, SourceId};
use nugget_graph::*;
use nugget_vault::*;

struct Fixture {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    graph: Arc<MemGraph>,
    vault: Arc<Vault>,
    sam: EntityId,
    acme: EntityId,
    works_at: ClaimId,
    prefers: ClaimId,
}

fn item(source: &str, key: &str) -> ItemRef {
    ItemRef {
        source: SourceId::new(source),
        key: ItemKey::new(key),
    }
}

fn ev(source: &str, key: &str, quote: &str) -> Evidence {
    Evidence {
        item: item(source, key),
        chunk_id: Some(1),
        quote: Some(quote.into()),
    }
}

fn claim(
    s: &EntityId,
    pred: &str,
    obj: ClaimObject,
    text: &str,
    evidence: Vec<Evidence>,
) -> NewClaim {
    NewClaim {
        subject: s.clone(),
        predicate: pred.into(),
        object: obj,
        text: text.into(),
        confidence: 0.9,
        status: ClaimStatus::Active,
        evidence,
        valid_from: None,
        valid_to: None,
    }
}

fn lookup(it: &ItemRef) -> Option<SourceInfo> {
    match it.key.as_str() {
        "msg-1" => Some(SourceInfo {
            title: Some("Lunch on Friday".into()),
            url: Some("https://mail.example/msg-1".into()),
            sensitive: false,
        }),
        "notes.md" => Some(SourceInfo {
            title: Some("Notes".into()),
            url: None,
            sensitive: false,
        }),
        "counselling.ics" => Some(SourceInfo {
            title: Some("Counselling session".into()),
            url: None,
            sensitive: true,
        }),
        _ => None,
    }
}

fn populate(g: &MemGraph) -> (EntityId, EntityId, ClaimId, ClaimId) {
    let mut sam = NewEntity::new(EntityKind::Person, "Samantha Lee");
    sam.aliases = vec!["Sam".into()];
    sam.identifiers = vec![Identifier::new("email", "sam@example.com")];
    let sam = g.create_entity(&sam).unwrap();
    let acme = g
        .create_entity(&NewEntity::new(EntityKind::Organization, "Acme"))
        .unwrap();
    let works_at = g
        .add_claim(&claim(
            &sam,
            "works_at",
            ClaimObject::Entity(acme.clone()),
            "Samantha works at Acme.",
            vec![ev("mail", "msg-1", "Sam from Acme here")],
        ))
        .unwrap();
    let prefers = g
        .add_claim(&claim(
            &sam,
            "prefers",
            ClaimObject::Literal("oat milk".into()),
            "Prefers oat milk in coffee.",
            vec![
                ev("files", "notes.md", "Sam: oat milk please"),
                ev("mail", "msg-1", "Sam from Acme here"),
            ],
        ))
        .unwrap();
    (sam, acme, works_at, prefers)
}

fn config() -> VaultConfig {
    VaultConfig {
        debounce: Duration::from_millis(100),
        ..VaultConfig::default()
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("vault");
    let graph = Arc::new(MemGraph::new());
    let (sam, acme, works_at, prefers) = populate(&graph);
    let vault = Arc::new(Vault::new(&root, graph.clone(), Arc::new(lookup), config()).unwrap());
    let root = vault.root().to_path_buf();
    Fixture {
        _dir: dir,
        root,
        graph,
        vault,
        sam,
        acme,
        works_at,
        prefers,
    }
}

impl Fixture {
    fn page(&self, id: &EntityId) -> std::path::PathBuf {
        self.vault.page_path(id).unwrap().expect("page")
    }
    fn read(&self, id: &EntityId) -> String {
        std::fs::read_to_string(self.page(id)).unwrap()
    }
    fn write(&self, id: &EntityId, text: &str) {
        std::fs::write(self.page(id), text).unwrap();
    }
    fn edit(&self, id: &EntityId, f: impl FnOnce(String) -> String) {
        let t = self.read(id);
        self.write(id, &f(t));
    }
    fn status(&self, c: ClaimId) -> ClaimStatus {
        self.graph.get_claim(c).unwrap().unwrap().status
    }
}

fn mask_updated(s: &str) -> String {
    s.lines()
        .map(|l| {
            if l.starts_with("updated: ") {
                "updated: \"<t>\""
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

const SAM_GOLDEN: &str = r#"---
nugget_id: "ent_000001"
kind: "person"
aliases: ["Sam"]
updated: "<t>"
---
<!-- nugget:begin header -->
# Samantha Lee
<!-- nugget:end header -->

<!-- nugget:begin facts -->
## Facts

### Prefers <!-- p:prefers -->
- Prefers oat milk in coffee. [^1][^2] <!-- c:2 -->

### Works at <!-- p:works_at -->
- Samantha works at [[Organization/Acme (ent_000002)|Acme]]. [^2] <!-- c:1 -->
<!-- nugget:end facts -->

<!-- nugget:begin related -->
## Related

- [[Organization/Acme (ent_000002)|Acme]]: works at
<!-- nugget:end related -->

<!-- nugget:begin sources -->
[^1]: [Notes](nugget://item/files/notes.md): "Sam: oat milk please"
[^2]: [Lunch on Friday](https://mail.example/msg-1): "Sam from Acme here"
<!-- nugget:end sources -->

## Notes

"#;

const ACME_GOLDEN: &str = r#"---
nugget_id: "ent_000002"
kind: "organization"
aliases: []
updated: "<t>"
---
<!-- nugget:begin header -->
# Acme
<!-- nugget:end header -->

<!-- nugget:begin facts -->
## Facts

_No facts yet._
<!-- nugget:end facts -->

<!-- nugget:begin related -->
## Related

- [[Person/Samantha Lee (ent_000001)|Samantha Lee]]: ← works at
<!-- nugget:end related -->

<!-- nugget:begin sources -->
<!-- nugget:end sources -->

## Notes

"#;

const README_GOLDEN: &str = r#"<!-- nugget:begin index -->
# Nugget vault

Generated from your Nugget knowledge graph. Every fact cites its sources; facts marked (you) came from your own edits. Text outside the generated blocks, including each page's Notes section, is yours and survives re-renders. Edit a fact line to correct it, or delete it to retract it.

## Person (1)

- [[Person/Samantha Lee (ent_000001)|Samantha Lee]]

## Organization (1)

- [[Organization/Acme (ent_000002)|Acme]]
<!-- nugget:end index -->

"#;

#[test]
fn golden_render() {
    let f = fixture();
    let r = f.vault.render_all().unwrap();
    assert_eq!(r.written.len(), 3, "{r:?}");
    let sam_rel = f.page(&f.sam);
    assert_eq!(sam_rel, f.root.join("Person/Samantha Lee (ent_000001).md"));
    assert_eq!(mask_updated(&f.read(&f.sam)), SAM_GOLDEN);
    assert_eq!(mask_updated(&f.read(&f.acme)), ACME_GOLDEN);
    assert_eq!(
        std::fs::read_to_string(f.root.join("README.md")).unwrap(),
        README_GOLDEN
    );
    let report = f.vault.validate().unwrap();
    assert!(report.is_ok(), "{report:?}");
    assert_eq!(report.facts, 2);
    assert_eq!(report.cited_facts, 2);
    assert_eq!(report.citation_coverage(), 1.0);
    let st = f.vault.status();
    assert_eq!(st.pages, 2);
    assert!(st.last_render.is_some() && st.conflicts.is_empty() && st.invalid.is_empty());
}

#[test]
fn identifiers_are_opt_in() {
    let dir = tempfile::tempdir().unwrap();
    let g = Arc::new(MemGraph::new());
    let (sam, ..) = populate(&g);
    let cfg = VaultConfig {
        include_identifiers: true,
        ..config()
    };
    let v = Vault::new(dir.path(), g, Arc::new(NoItems), cfg).unwrap();
    v.render_all().unwrap();
    let text = std::fs::read_to_string(v.page_path(&sam).unwrap().unwrap()).unwrap();
    assert!(text.contains("identifiers: {\"email\":[\"sam@example.com\"]}"));
    // Without a lookup, citations fall back to the item key and a nugget:// link.
    assert!(text.contains("[^1]: [notes.md](nugget://item/files/notes.md)"));
    assert!(v.validate().unwrap().is_ok());
}

#[test]
fn rendering_is_deterministic() {
    let f = fixture();
    f.vault.render_all().unwrap();
    // Same graph, second vault: byte-identical pages.
    let other = tempfile::tempdir().unwrap();
    let v2 = Vault::new(other.path(), f.graph.clone(), Arc::new(lookup), config()).unwrap();
    v2.render_all().unwrap();
    for id in [&f.sam, &f.acme] {
        let a = f.read(id);
        let b = std::fs::read_to_string(v2.page_path(id).unwrap().unwrap()).unwrap();
        assert_eq!(a, b);
    }
    // Re-rendering an unchanged graph writes nothing.
    let r = f.vault.render_all().unwrap();
    assert!(r.written.is_empty(), "{r:?}");
    assert!(r.edits.is_empty());
}

#[test]
fn user_notes_survive_rerenders() {
    let f = fixture();
    f.vault.render_all().unwrap();
    f.edit(&f.sam, |t| {
        t.replace(
            "<!-- nugget:begin facts -->",
            "Met at the 2024 offsite.\n\n<!-- nugget:begin facts -->",
        ) + "Call her about the Q3 plan.\n- [ ] send deck\n"
    });
    // The graph changes: a new fact with new evidence.
    let likes = f
        .graph
        .add_claim(&claim(
            &f.sam,
            "likes",
            ClaimObject::Literal("hiking".into()),
            "Likes hiking.",
            vec![ev("mail", "msg-9", "went hiking")],
        ))
        .unwrap();
    let r = f.vault.render_all().unwrap();
    assert!(r.edits.is_empty(), "notes are not facts: {r:?}");
    let t = f.read(&f.sam);
    assert!(t.contains("Met at the 2024 offsite.\n\n<!-- nugget:begin facts -->"));
    assert!(t.ends_with("## Notes\n\nCall her about the Q3 plan.\n- [ ] send deck\n"));
    assert!(t.contains(&format!("- Likes hiking. [^1] <!-- c:{likes} -->")));
    assert!(f.vault.status().conflicts.is_empty());
    // Free text inside the facts block is kept next to the fact it followed.
    f.edit(&f.sam, |t| {
        t.replace(
            "<!-- c:1 -->\n",
            "<!-- c:1 -->\n  - (she leads the platform team)\n",
        )
    });
    f.graph
        .add_evidence(f.works_at, &ev("mail", "msg-10", "Acme's Sam"))
        .unwrap();
    f.vault.render_all().unwrap();
    let t = f.read(&f.sam);
    assert!(
        t.contains("<!-- c:1 -->\n  - (she leads the platform team)\n"),
        "{t}"
    );
    assert!(f.vault.validate().unwrap().is_ok());
}

#[test]
fn edited_fact_becomes_user_asserted() {
    let f = fixture();
    f.vault.render_all().unwrap();
    f.edit(&f.sam, |t| {
        t.replace(
            "- Prefers oat milk in coffee. [^1][^2] <!-- c:2 -->",
            "- Prefers soy milk in coffee. [^1][^2] <!-- c:2 -->",
        )
    });
    let rep = f.vault.apply_user_edits(f.page(&f.sam)).unwrap();
    assert_eq!(rep.superseded, vec![f.prefers]);
    assert_eq!(rep.asserted.len(), 1);
    assert_eq!(f.status(f.prefers), ClaimStatus::Superseded);
    let new = f.graph.get_claim(rep.asserted[0]).unwrap().unwrap();
    assert_eq!(new.status, ClaimStatus::UserAsserted);
    assert_eq!(new.predicate, "prefers");
    assert_eq!(new.text, "Prefers soy milk in coffee.");
    assert!(new.evidence.is_empty());
    // The page was re-rendered: the fact is now marked as yours and carries the new claim id.
    let t = f.read(&f.sam);
    assert!(t.contains(&format!(
        "- Prefers soy milk in coffee. (you) <!-- c:{} -->",
        new.id
    )));
    assert!(!t.contains("oat milk"));
    let report = f.vault.validate().unwrap();
    assert!(report.is_ok(), "{report:?}");
    assert_eq!(report.user_facts, 1);
    // Applying again is a no-op.
    assert!(f.vault.apply_user_edits(f.page(&f.sam)).unwrap().is_empty());
}

#[test]
fn added_fact_lines_and_links() {
    let f = fixture();
    f.vault.render_all().unwrap();
    f.edit(&f.sam, |t| {
        t.replace(
            "<!-- nugget:end facts -->",
            "\n### Mentoring\n- Mentors [[Organization/Acme (ent_000002)|Acme]] interns\n\n### Hobbies\n- Plays chess\n<!-- nugget:end facts -->",
        )
    });
    let rep = f.vault.apply_user_edits(f.page(&f.sam)).unwrap();
    assert_eq!(rep.asserted.len(), 2);
    let a = f.graph.get_claim(rep.asserted[0]).unwrap().unwrap();
    assert_eq!(a.predicate, "mentoring");
    assert_eq!(a.object, ClaimObject::Entity(f.acme.clone()));
    assert_eq!(a.text, "Mentors Acme interns");
    let b = f.graph.get_claim(rep.asserted[1]).unwrap().unwrap();
    assert_eq!(b.predicate, "hobbies");
    assert_eq!(b.object, ClaimObject::Literal("Plays chess".into()));
    let t = f.read(&f.sam);
    assert!(
        t.contains("### Hobbies <!-- p:hobbies -->\n- Plays chess (you)"),
        "{t}"
    );
    // Acme's page now shows the new inbound relation too.
    assert!(
        f.read(&f.acme).contains("← mentoring, ← works at"),
        "{}",
        f.read(&f.acme)
    );
    assert!(f.vault.validate().unwrap().is_ok());
}

#[test]
fn deleted_fact_is_rejected_and_stays_rejected_after_re_extraction() {
    let f = fixture();
    f.vault.render_all().unwrap();
    f.edit(&f.sam, |t| {
        t.lines()
            .filter(|l| !l.contains("<!-- c:1 -->"))
            .collect::<Vec<_>>()
            .join("\n")
    });
    let r = f.vault.render_all().unwrap();
    assert_eq!(r.edits.retracted, vec![f.works_at]);
    assert_eq!(f.status(f.works_at), ClaimStatus::Rejected);
    assert_eq!(f.status(f.prefers), ClaimStatus::Active);
    let t = f.read(&f.sam);
    assert!(!t.contains("works at"));
    // The other side of the relation was re-rendered as well.
    assert!(f.read(&f.acme).contains("_No related entities yet._"));
    assert!(f.vault.validate().unwrap().is_ok());

    // Its source is re-extracted (retracted, then the same fact found again): it does not come
    // back.
    f.graph.retract_item(&item("mail", "msg-1")).unwrap();
    let again = f
        .graph
        .add_claim(&claim(
            &f.sam,
            "works_at",
            ClaimObject::Entity(f.acme.clone()),
            "Samantha works at Acme.",
            vec![ev("mail", "msg-1", "Sam from Acme here")],
        ))
        .unwrap();
    assert_eq!(again, f.works_at);
    assert_eq!(f.status(f.works_at), ClaimStatus::Rejected);
    f.vault.render_all().unwrap();
    assert!(!f.read(&f.sam).contains("works at"), "{}", f.read(&f.sam));
}

#[test]
fn split_moves_fact_lines_to_the_new_page() {
    let f = fixture();
    f.vault.render_all().unwrap();
    let (_, seq) = f.graph.changes_since(0, 0).unwrap();
    let mut spec = NewEntity::new(EntityKind::Person, "Sam Lee");
    spec.aliases = vec!["Sam".into()];
    let sam2 = f.graph.split(&f.sam, &[f.works_at], &spec).unwrap();
    let r = f.vault.render_changed(seq).unwrap();
    assert!(
        r.edits.is_empty(),
        "a split is not a user edit: {:?}",
        r.edits
    );
    assert_eq!(f.status(f.works_at), ClaimStatus::Active);
    let old = f.read(&f.sam);
    let new = f.read(&sam2);
    assert!(!old.contains("<!-- c:1 -->"), "{old}");
    assert!(new.contains("<!-- c:1 -->"), "{new}");
    assert!(new.contains("# Sam Lee"));
    assert!(old.contains("<!-- c:2 -->"));
    assert!(f.vault.validate().unwrap().is_ok());
}

#[test]
fn an_edit_to_a_fact_split_off_since_the_last_render_lands_on_its_new_entity() {
    let f = fixture();
    f.vault.render_all().unwrap();
    // The user edits the works_at line while (before the next render) the fact is split off.
    f.edit(&f.sam, |t| {
        t.lines()
            .map(|l| {
                if l.contains("<!-- c:1 -->") {
                    "- Works at Acme as CTO <!-- c:1 -->".to_string()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    let sam2 = f
        .graph
        .split(
            &f.sam,
            &[f.works_at],
            &NewEntity::new(EntityKind::Person, "Sam Lee"),
        )
        .unwrap();
    let r = f.vault.render_all().unwrap();
    assert_eq!(r.edits.superseded, vec![f.works_at]);
    assert_eq!(r.edits.asserted.len(), 1);
    let asserted = f.graph.get_claim(r.edits.asserted[0]).unwrap().unwrap();
    assert_eq!(asserted.subject, sam2, "the edit follows the fact");
    assert_eq!(asserted.status, ClaimStatus::UserAsserted);
    assert!(f.read(&sam2).contains("Works at Acme as CTO"));
    assert!(!f.read(&f.sam).contains("Works at Acme as CTO"));
}

#[test]
fn conflicts_keep_user_text_and_write_a_sidecar() {
    let f = fixture();
    f.vault.render_all().unwrap();
    // The user edits a generated block...
    f.edit(&f.sam, |t| {
        t.replace(
            "- [[Organization/Acme (ent_000002)|Acme]]: works at\n",
            "- [[Organization/Acme (ent_000002)|Acme]]: works at (until 2025, I think)\n",
        )
    });
    // ...and the graph changes the same block.
    let bob = f
        .graph
        .create_entity(&NewEntity::new(EntityKind::Person, "Bob"))
        .unwrap();
    f.graph
        .add_claim(&claim(
            &f.sam,
            "knows",
            ClaimObject::Entity(bob.clone()),
            "Sam knows Bob.",
            vec![ev("mail", "msg-3", "Sam and Bob")],
        ))
        .unwrap();
    let r = f.vault.render_all().unwrap();
    let rel = "Person/Samantha Lee (ent_000001).md";
    assert_eq!(r.conflicts, vec![rel.to_string()]);
    assert_eq!(f.vault.status().conflicts, vec![rel.to_string()]);
    let t = f.read(&f.sam);
    assert!(
        t.contains("works at (until 2025, I think)"),
        "user text kept"
    );
    // Non-conflicting blocks still update.
    assert!(t.contains("- Sam knows [[Person/Bob (ent_000003)|Bob]]."));
    let sidecar = std::fs::read_to_string(f.root.join(".nugget/conflicts").join(rel)).unwrap();
    assert!(sidecar.starts_with("<!-- nugget: generated version of"));
    assert!(sidecar.contains("- [[Person/Bob (ent_000003)|Bob]]: knows"));
    // Still a conflict on the next render while the user's version differs.
    f.vault.render_all().unwrap();
    assert_eq!(f.vault.status().conflicts, vec![rel.to_string()]);
    // The user resolves by taking the generated block; the conflict clears.
    let generated = nugget_block(&sidecar, "related");
    f.edit(&f.sam, |t| {
        let mine = nugget_block(&t, "related");
        t.replace(&mine, &generated)
    });
    f.vault.render_all().unwrap();
    assert!(f.vault.status().conflicts.is_empty());
    assert!(!f.root.join(".nugget/conflicts").join(rel).exists());
}

fn nugget_block(text: &str, name: &str) -> String {
    let b = format!("<!-- nugget:begin {name} -->");
    let e = format!("<!-- nugget:end {name} -->");
    let s = text.find(&b).unwrap();
    let end = text.find(&e).unwrap() + e.len();
    text[s..end].to_owned()
}

#[test]
fn rename_moves_the_page_and_keeps_links() {
    let f = fixture();
    f.vault.render_all().unwrap();
    let old_acme = f.page(&f.acme);
    f.edit(&f.sam, |t| {
        t + "Follow up with [[Organization/Acme (ent_000002)|them]] and [[Acme (ent_000002)]].\n"
    });
    f.edit(&f.acme, |t| t + "Acme notes stay with Acme.\n");
    f.graph
        .update_entity(
            &f.acme,
            &EntityPatch {
                name: Some("Acme Corp".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let r = f.vault.render_changed(0).unwrap();
    assert!(r.high_water > 0);
    let new_acme = f.page(&f.acme);
    assert_eq!(
        new_acme,
        f.root.join("Organization/Acme Corp (ent_000002).md")
    );
    assert!(!old_acme.exists());
    assert_eq!(r.moved.len(), 1);
    assert!(f.read(&f.acme).contains("Acme notes stay with Acme."));
    let sam = f.read(&f.sam);
    assert!(sam.contains(
        "- Samantha works at [[Organization/Acme Corp (ent_000002)|Acme]]. [^2] <!-- c:1 -->"
    ));
    assert!(sam.contains("- [[Organization/Acme Corp (ent_000002)|Acme Corp]]: works at"));
    assert!(sam.contains(
        "Follow up with [[Organization/Acme Corp (ent_000002)|them]] and [[Acme Corp (ent_000002)]]."
    ));
    // Old name became an alias.
    assert!(f.read(&f.acme).contains("aliases: [\"Acme\"]"));
    let report = f.vault.validate().unwrap();
    assert!(report.is_ok(), "{report:?}");
}

#[test]
fn merged_entity_becomes_a_redirect_and_notes_move() {
    let f = fixture();
    let dup = f
        .graph
        .create_entity(&NewEntity::new(EntityKind::Person, "S. Lee"))
        .unwrap();
    f.graph
        .add_claim(&claim(
            &dup,
            "lives_in",
            ClaimObject::Literal("Lisbon".into()),
            "Lives in Lisbon.",
            vec![ev("mail", "msg-4", "back in Lisbon")],
        ))
        .unwrap();
    let r = f.vault.render_changed(0).unwrap();
    let seq = r.high_water;
    assert!(seq > 0);
    let dup_path = f.page(&dup);
    f.edit(&dup, |t| t + "Prefers calls after 5pm.\n");
    f.graph.merge(&f.sam, &dup).unwrap();
    let r = f.vault.render_changed(seq).unwrap();
    assert_eq!(r.stubs.len(), 1, "{r:?}");
    // The old page is a redirect stub that links to the survivor.
    let stub = std::fs::read_to_string(&dup_path).unwrap();
    assert!(stub.contains("merged_into: \"ent_000001\""), "{stub}");
    assert!(stub.contains("[[Person/Samantha Lee (ent_000001)|Samantha Lee]]"));
    assert!(!stub.contains("Prefers calls after 5pm."));
    // Notes moved, and the merged entity's facts now render on the survivor's page.
    let sam = f.read(&f.sam);
    assert!(
        sam.contains("### Notes moved from S. Lee\n\nPrefers calls after 5pm.\n"),
        "{sam}"
    );
    assert!(sam.contains("Lives in Lisbon."));
    assert!(sam.contains("aliases: [\"Sam\",\"S. Lee\"]"));
    // The old id resolves to the survivor's page.
    assert_eq!(f.vault.page_path(&dup).unwrap().unwrap(), f.page(&f.sam));
    assert_eq!(f.vault.status().pages, 2);
    let report = f.vault.validate().unwrap();
    assert!(report.is_ok(), "{report:?}");
    // Stable afterwards: notes are not moved twice.
    f.vault.render_all().unwrap();
    assert_eq!(
        f.read(&f.sam).matches("Prefers calls after 5pm.").count(),
        1
    );
    assert!(
        !f.vault
            .render_all()
            .unwrap()
            .written
            .iter()
            .any(|w| w.contains("Samantha"))
    );
}

#[test]
fn validators_catch_missing_citations_and_broken_links() {
    let f = fixture();
    f.vault.render_all().unwrap();
    let original = f.read(&f.sam);
    f.edit(&f.sam, |t| {
        t.replace(
            "- Prefers oat milk in coffee. [^1][^2] <!-- c:2 -->",
            "- Prefers oat milk in coffee. <!-- c:2 -->",
        )
        .replace(
            "<!-- nugget:end related -->",
            "- [[Person/Nobody (ent_999999)|Nobody]]: knows\n<!-- nugget:end related -->",
        )
        .replace("kind: \"person\"", "kind: \"robot\"")
    });
    let report = f.vault.validate().unwrap();
    let kinds: Vec<IssueKind> = report.issues.iter().map(|i| i.kind).collect();
    assert!(kinds.contains(&IssueKind::MissingCitation), "{report:?}");
    assert!(kinds.contains(&IssueKind::BrokenLink));
    assert!(kinds.contains(&IssueKind::BadFrontmatter));
    assert!(report.citation_coverage() < 1.0);
    let rel = "Person/Samantha Lee (ent_000001).md".to_string();
    assert_eq!(f.vault.status().invalid, vec![rel.clone()]);
    let miss = report
        .issues
        .iter()
        .find(|i| i.kind == IssueKind::MissingCitation)
        .unwrap();
    let line = f
        .read(&f.sam)
        .lines()
        .nth(miss.line.unwrap() - 1)
        .unwrap()
        .to_owned();
    assert!(line.contains("<!-- c:2 -->"));

    // A "(you)" tag on a claim that is not user-asserted does not count as a citation.
    f.write(&f.sam, &original);
    f.edit(&f.sam, |t| {
        t.replace("[^1][^2] <!-- c:2 -->", "(you) <!-- c:2 -->")
    });
    let report = f.vault.validate().unwrap();
    assert_eq!(report.issues.len(), 1, "{report:?}");

    // The pure validator works on any text.
    let r = validate_page("x.md", "- loose fact\n", &|_| true);
    assert!(r.issues.iter().any(|i| i.kind == IssueKind::BadFrontmatter));
}

#[test]
fn hostile_names_stay_inside_the_vault() {
    let dir = tempfile::tempdir().unwrap();
    let g = Arc::new(MemGraph::new());
    let names = [
        "../../escape",
        "CON",
        "a/b\\c",
        ".hidden",
        &"x".repeat(400),
        "Ünïcödé\u{202E}",
    ];
    let ids: Vec<EntityId> = names
        .iter()
        .map(|n| {
            g.create_entity(&NewEntity::new(EntityKind::Topic, *n))
                .unwrap()
        })
        .collect();
    let v = Vault::new(dir.path().join("v"), g, Arc::new(NoItems), config()).unwrap();
    v.render_all().unwrap();
    let root = v.root().to_path_buf();
    for id in &ids {
        let p = v.page_path(id).unwrap().unwrap();
        assert!(p.starts_with(root.join("Topic")), "{p:?}");
        assert!(p.exists());
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(name.len() < 200 && !name.starts_with('.'));
    }
    assert!(!dir.path().join("escape").exists());
    let con = v.page_path(&ids[1]).unwrap().unwrap();
    assert_eq!(con.file_name().unwrap(), "_CON (ent_000002).md");
    assert!(v.validate().unwrap().is_ok());
}

#[cfg(unix)]
#[test]
fn symlinked_pages_are_never_followed() {
    let f = fixture();
    f.vault.render_all().unwrap();
    let outside = f._dir.path().join("secret.txt");
    std::fs::write(&outside, "do not touch\n").unwrap();
    let page = f.page(&f.acme);
    std::fs::remove_file(&page).unwrap();
    std::os::unix::fs::symlink(&outside, &page).unwrap();
    f.graph
        .update_entity(
            &f.acme,
            &EntityPatch {
                attributes: Some(serde_json::json!({"x": 1})),
                ..Default::default()
            },
        )
        .unwrap();
    let r = f.vault.render_all().unwrap();
    assert!(r.skipped.iter().any(|s| s.contains("Acme")), "{r:?}");
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "do not touch\n");
    assert!(f.vault.apply_user_edits(&page).is_ok());
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "do not touch\n");

    // A symlinked kind directory is refused too.
    let elsewhere = f._dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::remove_dir_all(f.root.join("Person")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, f.root.join("Person")).unwrap();
    let r = f.vault.render_all().unwrap();
    assert!(r.skipped.iter().any(|s| s.starts_with("Person/")), "{r:?}");
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    // Paths outside the vault are rejected.
    assert!(f.vault.apply_user_edits(&outside).is_err());
}

fn wait_for(deadline: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    f()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watcher_applies_edits() {
    let f = fixture();
    f.vault.render_all().unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let v = f.vault.clone();
    let c = cancel.clone();
    let task = tokio::spawn(async move { v.watch(c).await });

    // Our own writes are ignored: a render while watching applies no edits.
    tokio::time::sleep(Duration::from_millis(300)).await;
    f.graph
        .add_evidence(f.works_at, &ev("mail", "msg-7", "Acme badge"))
        .unwrap();
    f.vault.render_all().unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(f.status(f.works_at), ClaimStatus::Active);
    assert_eq!(f.status(f.prefers), ClaimStatus::Active);

    f.edit(&f.sam, |t| {
        t.lines()
            .filter(|l| !l.contains("<!-- c:2 -->"))
            .collect::<Vec<_>>()
            .join("\n")
    });
    let g = f.graph.clone();
    let p = f.prefers;
    let applied = tokio::task::spawn_blocking(move || {
        wait_for(Duration::from_secs(15), || {
            g.get_claim(p).unwrap().unwrap().status == ClaimStatus::Rejected
        })
    })
    .await
    .unwrap();
    assert!(applied, "watcher did not apply the deletion");
    assert_eq!(f.status(f.works_at), ClaimStatus::Active);
    cancel.cancel();
    task.await.unwrap().unwrap();
}

#[cfg(feature = "store")]
#[test]
fn citations_use_store_titles_and_urls() {
    let store = nugget_store::Store::open_in_memory().unwrap();
    let mut it = nugget_store::NewItem::new("mail", "msg-1", "h1");
    it.title = Some("Quarterly [draft]".into());
    it.metadata = serde_json::json!({"url": "https://mail.example/m?id=1 (x)", "sensitive": false});
    store.upsert_item(&it).unwrap();
    // Sensitivity unknown: counts as sensitive (`nugget_retrieve::is_sensitive`), so the note is
    // never cited.
    let mut note = nugget_store::NewItem::new("files", "notes.md", "h2");
    note.title = Some("Notes".into());
    store.upsert_item(&note).unwrap();
    let g = Arc::new(MemGraph::new());
    let (sam, ..) = populate(&g);
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::new(dir.path(), g, Arc::new(store), config()).unwrap();
    v.render_all().unwrap();
    let t = std::fs::read_to_string(v.page_path(&sam).unwrap().unwrap()).unwrap();
    assert!(
        t.contains("[^1]: [Quarterly (draft)](https://mail.example/m?id=1%20%28x%29): \"Sam from Acme here\""),
        "{t}"
    );
    assert!(t.contains("- Prefers oat milk in coffee. [^1] <!--"), "{t}");
    assert!(
        !t.contains("notes.md") && !t.contains("oat milk please"),
        "{t}"
    );
}

#[test]
fn state_survives_reopen() {
    let f = fixture();
    f.vault.render_all().unwrap();
    f.edit(&f.sam, |t| {
        t.replace("<!-- c:1 -->\n", "<!-- c:1 -->\nextra\n")
    });
    let root: &Path = &f.root;
    let v2 = Vault::new(root, f.graph.clone(), Arc::new(lookup), config()).unwrap();
    // Edits made while no vault was running are found by fingerprint.
    let rep = v2.apply_pending_edits().unwrap();
    assert!(rep.is_empty());
    assert_eq!(v2.status().pages, 2);
    assert!(
        v2.render_all()
            .unwrap()
            .written
            .iter()
            .all(|w| !w.contains("Acme"))
    );
}

#[test]
fn lost_manifest_still_applies_edits() {
    let f = fixture();
    f.vault.render_all().unwrap();
    std::fs::remove_file(f.root.join(".nugget/state.json")).unwrap();
    f.edit(&f.sam, |t| {
        t.lines()
            .filter(|l| !l.contains("<!-- c:2 -->"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\nkeep me\n"
    });
    let v2 = Vault::new(&f.root, f.graph.clone(), Arc::new(lookup), config()).unwrap();
    let r = v2.render_all().unwrap();
    assert_eq!(r.edits.retracted, vec![f.prefers]);
    let t = std::fs::read_to_string(v2.page_path(&f.sam).unwrap().unwrap()).unwrap();
    assert!(t.contains("keep me") && !t.contains("oat milk"));
    assert!(v2.validate().unwrap().is_ok());
}

#[test]
fn notes_are_written_inside_the_vault_and_never_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let g = Arc::new(MemGraph::new());
    let v = Vault::new(dir.path().join("v"), g, Arc::new(NoItems), config()).unwrap();
    let root = v.root().to_path_buf();
    let a = v
        .write_note("Notes", "Prep: Acme / kickoff", "# Prep\n\n- agenda\n")
        .unwrap();
    assert_eq!(a, root.join("Notes").join("Prep- Acme - kickoff.md"));
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "# Prep\n\n- agenda\n");
    let b = v
        .write_note("Notes", "Prep: Acme / kickoff", "second")
        .unwrap();
    assert_eq!(b, root.join("Notes").join("Prep- Acme - kickoff 2.md"));
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "# Prep\n\n- agenda\n");
    // Hostile names stay inside the vault.
    let c = v.write_note("../..", "../../escape", "x").unwrap();
    assert!(c.starts_with(&root), "{c:?}");
    assert!(!dir.path().join("escape.md").exists());
    // Notes are not pages: rendering and validation leave them alone.
    v.render_all().unwrap();
    assert!(a.exists() && b.exists());
    assert!(v.validate().unwrap().is_ok());
}

// ---- sensitive items (GG5-3) ----

/// Every file under `dir` (pages, index and `.nugget/` bases), as `(path, text)`.
fn all_files(dir: &Path) -> Vec<(std::path::PathBuf, String)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(all_files(&p));
        } else {
            out.push((p.clone(), std::fs::read_to_string(&p).unwrap_or_default()));
        }
    }
    out
}

fn assert_nowhere(root: &Path, needles: &[&str]) {
    for (p, text) in all_files(root) {
        for n in needles {
            assert!(!text.contains(n), "{n:?} in {}:\n{text}", p.display());
            assert!(
                !p.to_string_lossy().contains(n),
                "{n:?} in path {}",
                p.display()
            );
        }
    }
}

/// The counselling invite: Jonas organised a session Sam attended. Every fact about the session
/// and Jonas comes from the (sensitive) invite only; Sam's "likes" fact has evidence from both
/// the invite and a non-sensitive note. Returns (jonas, session, place, likes).
fn add_counselling(g: &MemGraph, sam: &EntityId) -> (EntityId, EntityId, EntityId, ClaimId) {
    let jonas = g
        .create_entity(&NewEntity::new(EntityKind::Person, "Jonas Albers"))
        .unwrap();
    let session = g
        .create_entity(&NewEntity::new(EntityKind::Event, "Counselling session"))
        .unwrap();
    let place = g
        .create_entity(&NewEntity::new(EntityKind::Place, "Quay Lane clinic"))
        .unwrap();
    let invite = |q: &str| ev("files", "counselling.ics", q);
    g.add_claim(&claim(
        &jonas,
        "organized",
        ClaimObject::Entity(session.clone()),
        "Jonas Albers organized Counselling session.",
        vec![invite("Organizer: Jonas Albers <jonas@albers.example>")],
    ))
    .unwrap();
    g.add_claim(&claim(
        sam,
        "attended",
        ClaimObject::Entity(session.clone()),
        "Samantha attended Counselling session.",
        vec![invite("Attendee: Samantha Lee")],
    ))
    .unwrap();
    let likes = g
        .add_claim(&claim(
            sam,
            "likes",
            ClaimObject::Literal("long walks".into()),
            "Likes long walks.",
            vec![
                ev("files", "notes.md", "Sam: long walks"),
                invite("SECRET: walks help with the sleep diary"),
            ],
        ))
        .unwrap();
    // An entity known only from a mention in the invite.
    g.add_mention(&Mention {
        entity: place.clone(),
        item: item("files", "counselling.ics"),
        chunk_id: None,
        surface: "Quay Lane".into(),
        confidence: 1.0,
    })
    .unwrap();
    (jonas, session, place, likes)
}

#[test]
fn sensitive_only_entities_and_facts_stay_out_of_the_vault() {
    let f = fixture();
    let (jonas, session, place, likes) = add_counselling(&f.graph, &f.sam);
    f.vault.render_all().unwrap();
    for id in [&jonas, &session, &place] {
        assert_eq!(f.vault.page_path(id).unwrap(), None, "{id}");
    }
    // Not a line, footnote, link, index entry or merge base anywhere in the vault.
    assert_nowhere(
        &f.root,
        &[
            "Jonas",
            "Counselling",
            "Quay Lane",
            "counselling.ics",
            "SECRET",
            "attended",
        ],
    );
    // The mixed fact stays, citing only the non-sensitive note.
    let sam = f.read(&f.sam);
    let line = sam
        .lines()
        .find(|l| l.contains(&format!("<!-- c:{likes} -->")))
        .unwrap_or_else(|| panic!("no line for the mixed fact:\n{sam}"));
    assert!(line.starts_with("- Likes long walks. [^"), "{line}");
    assert!(sam.contains("[Notes](nugget://item/files/notes.md): \"Sam: long walks\""));
    assert_eq!(f.vault.status().pages, 2);
    let report = f.vault.validate().unwrap();
    assert!(report.is_ok(), "{report:?}");
}

/// A lookup whose invite turns sensitive (or back) at runtime, and that may count unknown items
/// as sensitive, as the store does.
struct Switch {
    invite_sensitive: std::sync::atomic::AtomicBool,
    missing_sensitive: bool,
}

impl ItemLookup for Switch {
    fn source_info(&self, it: &ItemRef) -> nugget_core::Result<Option<SourceInfo>> {
        Ok(match it.key.as_str() {
            "counselling.ics" => Some(SourceInfo {
                title: Some("Counselling session".into()),
                url: None,
                sensitive: self
                    .invite_sensitive
                    .load(std::sync::atomic::Ordering::SeqCst),
            }),
            _ => lookup(it),
        })
    }

    fn missing_is_sensitive(&self) -> bool {
        self.missing_sensitive
    }
}

#[test]
fn a_page_whose_sources_turn_sensitive_becomes_a_stub_and_comes_back() {
    use std::sync::atomic::Ordering;
    let dir = tempfile::tempdir().unwrap();
    let g = Arc::new(MemGraph::new());
    let (sam, ..) = populate(&g);
    let (jonas, session, ..) = add_counselling(&g, &sam);
    let items = Arc::new(Switch {
        invite_sensitive: false.into(),
        missing_sensitive: false,
    });
    let v = Vault::new(dir.path().join("v"), g.clone(), items.clone(), config()).unwrap();
    let root = v.root().to_path_buf();
    v.render_all().unwrap();
    let page = v
        .page_path(&jonas)
        .unwrap()
        .expect("page while not sensitive");
    let text = std::fs::read_to_string(&page).unwrap();
    assert!(text.contains("Organizer: Jonas Albers"), "{text}");
    std::fs::write(&page, format!("{text}my own note\n")).unwrap();
    v.apply_pending_edits().unwrap();

    let session_page = v.page_path(&session).unwrap().expect("session page");

    items.invite_sensitive.store(true, Ordering::SeqCst);
    let report = v.render_all().unwrap();
    // Jonas's page keeps the user's note, in place, under a neutral heading.
    let stub = std::fs::read_to_string(&page).unwrap();
    assert!(stub.contains("no longer in your knowledge graph"), "{stub}");
    assert!(stub.contains("# Hidden page"), "{stub}");
    assert!(stub.contains("my own note"), "{stub}");
    for leak in [
        "Jonas",
        "Organizer",
        "Counselling",
        "albers.example",
        "SECRET",
    ] {
        assert!(!stub.contains(leak), "{leak} in stub:\n{stub}");
    }
    // Session and place pages had nothing of the user's: gone, merge bases included.
    assert!(!session_page.exists(), "{}", session_page.display());
    assert_eq!(report.removed.len(), 2, "session and place: {report:?}");
    assert_nowhere(
        &root.join(".nugget"),
        &["Counselling", "Organizer", "SECRET"],
    );
    assert_eq!(v.page_path(&jonas).unwrap(), None);
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    assert!(
        !readme.contains("Jonas") && !readme.contains("Counselling"),
        "{readme}"
    );
    let sam_page = std::fs::read_to_string(v.page_path(&sam).unwrap().unwrap()).unwrap();
    assert!(
        !sam_page.contains("Counselling") && !sam_page.contains("SECRET"),
        "{sam_page}"
    );
    assert!(sam_page.contains("Likes long walks."), "{sam_page}");
    // No live page links to the stubs (validation would flag a dangling link).
    assert!(v.validate().unwrap().is_ok());
    assert_eq!(v.status().pages, 2);

    // Not sensitive any more: the pages come back.
    items.invite_sensitive.store(false, Ordering::SeqCst);
    v.render_all().unwrap();
    let back = std::fs::read_to_string(v.page_path(&jonas).unwrap().expect("page back")).unwrap();
    assert!(back.contains("Organizer: Jonas Albers"), "{back}");
    assert!(v.page_path(&session).unwrap().is_some());
}

#[test]
fn unknown_items_count_as_sensitive_when_the_lookup_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let g = Arc::new(MemGraph::new());
    let sam = g
        .create_entity(&NewEntity::new(EntityKind::Person, "Sam Lee"))
        .unwrap();
    let ghost = g
        .create_entity(&NewEntity::new(EntityKind::Person, "Ghost Person"))
        .unwrap();
    g.add_claim(&claim(
        &sam,
        "prefers",
        ClaimObject::Literal("tea".into()),
        "Prefers tea.",
        vec![
            ev("files", "notes.md", "Sam: tea"),
            ev("files", "gone.md", "gone quote"),
        ],
    ))
    .unwrap();
    g.add_claim(&claim(
        &ghost,
        "likes",
        ClaimObject::Literal("chess".into()),
        "Likes chess.",
        vec![ev("files", "gone.md", "gone quote")],
    ))
    .unwrap();
    let items = Arc::new(Switch {
        invite_sensitive: true.into(),
        missing_sensitive: true,
    });
    let v = Vault::new(dir.path().join("v"), g, items, config()).unwrap();
    v.render_all().unwrap();
    assert_eq!(v.page_path(&ghost).unwrap(), None);
    let root = v.root().to_path_buf();
    assert_nowhere(&root, &["Ghost", "gone.md", "gone quote", "chess"]);
    let t = std::fs::read_to_string(v.page_path(&sam).unwrap().unwrap()).unwrap();
    assert!(
        t.contains("Prefers tea. [^1]") && t.contains("\"Sam: tea\""),
        "{t}"
    );
}

// ---------------------------------------------------------------- GG5 regressions

fn replace_line(t: &str, marker: &str, with: &str) -> String {
    t.lines()
        .map(|l| {
            if l.contains(marker) {
                with.to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn works_at_again(f: &Fixture, quote: &str) -> ClaimId {
    f.graph.retract_item(&item("mail", "msg-1")).unwrap();
    f.graph
        .add_claim(&claim(
            &f.sam,
            "works_at",
            ClaimObject::Entity(f.acme.clone()),
            "Samantha works at Acme.",
            vec![ev("mail", "msg-1", quote)],
        ))
        .unwrap()
}

/// GG5-4: an edited fact line stays edited when its unchanged source is extracted again; a
/// source that now says it differently is a new fact.
#[test]
fn edited_fact_is_not_revived_by_re_extracting_its_unchanged_source() {
    let f = fixture();
    f.vault.render_all().unwrap();
    f.edit(&f.sam, |t| {
        replace_line(
            &t,
            "<!-- c:1 -->",
            "- Samantha is an Acme engineer (fixed by me) <!-- c:1 -->",
        )
    });
    let r = f.vault.render_all().unwrap();
    assert_eq!(r.edits.superseded, vec![f.works_at]);
    let user = r.edits.asserted[0];

    // Re-extraction, both ways the extractor writes it (a new claim, or evidence on the
    // retracted one).
    assert_eq!(works_at_again(&f, "Sam from Acme here"), f.works_at);
    assert_eq!(f.status(f.works_at), ClaimStatus::Superseded);
    f.graph.retract_item(&item("mail", "msg-1")).unwrap();
    f.graph
        .add_evidence(f.works_at, &ev("mail", "msg-1", "Sam from Acme here"))
        .unwrap();
    assert_eq!(f.status(f.works_at), ClaimStatus::Superseded);
    f.vault.render_all().unwrap();
    let t = f.read(&f.sam);
    assert!(!t.contains("Samantha works at"), "{t}");
    assert!(
        t.contains("- Samantha is an Acme engineer (fixed by me) (you)"),
        "{t}"
    );
    assert_eq!(f.status(user), ClaimStatus::UserAsserted);

    // The source changes what it says about the fact: normal rules again.
    let changed = works_at_again(&f, "Sam from Acme here, now its CTO");
    assert_eq!(f.status(changed), ClaimStatus::Active);
    f.vault.render_all().unwrap();
    assert!(f.read(&f.sam).contains("Samantha works at"));
}

/// GG5-5: a fact line retyped without its marker but with the same link keeps the user's words.
#[test]
fn retyped_fact_line_without_marker_keeps_the_users_text() {
    let f = fixture();
    f.vault.render_all().unwrap();
    f.edit(&f.sam, |t| {
        replace_line(
            &t,
            "<!-- c:1 -->",
            "- Samantha works at [[Organization/Acme (ent_000002)|Acme]] (retyped by me, no marker)",
        )
    });
    f.vault.render_all().unwrap();
    let c = f.graph.get_claim(f.works_at).unwrap().unwrap();
    assert_eq!(c.status, ClaimStatus::UserAsserted);
    assert_eq!(c.text, "Samantha works at Acme (retyped by me, no marker)");
    let t = f.read(&f.sam);
    assert!(t.contains("(retyped by me, no marker)"), "{t}");
    assert!(
        !t.contains("Samantha works at [[Organization/Acme (ent_000002)|Acme]]. "),
        "{t}"
    );
    // Re-extraction keeps the user's text and status.
    works_at_again(&f, "Sam from Acme here");
    let c = f.graph.get_claim(f.works_at).unwrap().unwrap();
    assert_eq!(c.status, ClaimStatus::UserAsserted);
    assert_eq!(c.text, "Samantha works at Acme (retyped by me, no marker)");
    f.vault.render_all().unwrap();
    assert!(f.read(&f.sam).contains("(retyped by me, no marker)"));
}

/// GG5-21: an editor that strips every HTML comment. The reworded line is an edit, the page is
/// not duplicated.
#[test]
fn page_without_markers_is_merged_not_duplicated() {
    let f = fixture();
    f.vault.render_all().unwrap();
    f.edit(&f.sam, |t| {
        let mut s = String::new();
        let mut rest = t.as_str();
        while let Some(i) = rest.find("<!--") {
            let j = rest[i..].find("-->").unwrap() + i + 3;
            s.push_str(rest[..i].strip_suffix(' ').unwrap_or(&rest[..i]));
            rest = &rest[j..];
        }
        s.push_str(rest);
        s.replace("Prefers oat milk", "Prefers soy milk") + "\nMy note on Sam.\n"
    });
    let r = f.vault.render_all().unwrap();
    assert_eq!(r.edits.superseded, vec![f.prefers]);
    assert_eq!(r.edits.asserted.len(), 1);
    assert!(r.edits.retracted.is_empty());
    let t = f.read(&f.sam);
    assert_eq!(t.matches("# Samantha Lee").count(), 1, "{t}");
    assert_eq!(t.matches("## Facts").count(), 1, "{t}");
    assert!(t.contains("<!-- nugget:begin facts -->"), "{t}");
    assert!(t.contains("Prefers soy milk in coffee. (you)"), "{t}");
    assert!(!t.contains("oat milk"), "{t}");
    assert!(t.contains("My note on Sam."), "{t}");
    assert!(f.vault.validate().unwrap().is_ok());
    // Stable afterwards.
    f.vault.render_all().unwrap();
    assert_eq!(f.read(&f.sam), t);
}

/// GG5-20: an alias added with YAML spacing is no conflict, and later aliases are written.
#[test]
fn alias_added_with_yaml_spacing_is_not_a_conflict() {
    let f = fixture();
    f.vault.render_all().unwrap();
    f.edit(&f.sam, |t| {
        t.replace("aliases: [\"Sam\"]", "aliases: [\"Sam\", \"Sammy\"]")
    });
    let r = f.vault.render_all().unwrap();
    assert_eq!(r.edits.aliases_added, vec!["Sammy".to_string()]);
    assert!(r.conflicts.is_empty(), "{r:?}");
    assert!(f.read(&f.sam).contains("aliases: [\"Sam\",\"Sammy\"]"));
    f.graph.add_alias(&f.sam, "Samuel").unwrap();
    let r = f.vault.render_all().unwrap();
    assert!(r.conflicts.is_empty(), "{r:?}");
    assert!(
        f.read(&f.sam)
            .contains("aliases: [\"Sam\",\"Sammy\",\"Samuel\"]"),
        "{}",
        f.read(&f.sam)
    );
}

/// GG5-12: splitting a wrongly merged entity back out redirects its old id and page to the
/// split-off entity, not to the one it was split from.
#[test]
fn split_after_a_mistaken_merge_redirects_the_old_page_to_the_split_off_entity() {
    let f = fixture();
    let dup = f
        .graph
        .create_entity(&NewEntity::new(EntityKind::Person, "S. Lee"))
        .unwrap();
    let lives = f
        .graph
        .add_claim(&claim(
            &dup,
            "lives_in",
            ClaimObject::Literal("Lisbon".into()),
            "Lives in Lisbon.",
            vec![ev("mail", "msg-4", "back in Lisbon")],
        ))
        .unwrap();
    let seq = f.vault.render_changed(0).unwrap().high_water;
    let dup_path = f.page(&dup);
    f.graph.merge(&f.sam, &dup).unwrap();
    let seq = f.vault.render_changed(seq).unwrap().high_water;
    assert!(
        std::fs::read_to_string(&dup_path)
            .unwrap()
            .contains("Samantha Lee")
    );
    let mut spec = NewEntity::new(EntityKind::Person, "S. Lee");
    spec.aliases = vec!["S. Lee".into()];
    let back = f.graph.split(&f.sam, &[lives], &spec).unwrap();
    assert_eq!(f.graph.get_entity(&dup).unwrap().unwrap().id, back);
    f.vault.render_changed(seq).unwrap();
    let page = f.read(&back);
    assert!(page.contains("# S. Lee"), "{page}");
    assert!(page.contains("Lives in Lisbon."), "{page}");
    assert!(!page.contains("merged_into"), "a real page: {page}");
    let stub = std::fs::read_to_string(&dup_path).unwrap();
    assert!(!stub.contains("Samantha Lee"), "{stub}");
    assert!(stub.contains(back.as_str()), "{stub}");
    assert_eq!(f.vault.page_path(&dup).unwrap().unwrap(), f.page(&back));
    assert!(!f.read(&f.sam).contains("Lisbon"));
    assert!(f.vault.validate().unwrap().is_ok());
}

// ---- sensitive items: upgrade, frontmatter, hidden pages, lookups (GG5-3 review) ----

/// A vault rendered before the sensitivity rules (no `rules` in its state) is rendered in full
/// once at the next `render_changed`, even with nothing in the change feed: pages, index
/// entries and footnotes of sensitive-only entities go.
#[test]
fn an_upgraded_vault_is_rendered_in_full_once() {
    use std::sync::atomic::Ordering;
    let dir = tempfile::tempdir().unwrap();
    let g = Arc::new(MemGraph::new());
    let (sam, ..) = populate(&g);
    let (jonas, session, ..) = add_counselling(&g, &sam);
    let items = Arc::new(Switch {
        invite_sensitive: false.into(),
        missing_sensitive: true,
    });
    let root = dir.path().join("v");
    let v = Vault::new(&root, g.clone(), items.clone(), config()).unwrap();
    let root = v.root().to_path_buf();
    assert!(!v.needs_full_render(), "nothing rendered under older rules");
    let first = v.render_changed(0).unwrap();
    assert!(first.written.iter().any(|p| p == "README.md"), "{first:?}");
    let mark = first.high_water;
    assert!(!v.needs_full_render());
    assert!(v.page_path(&session).unwrap().is_some());
    drop(v);

    // As an older version left it: same state, without the render-rules version.
    let state_path = root.join(".nugget/state.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    state.as_object_mut().unwrap().remove("rules");
    std::fs::write(&state_path, serde_json::to_string(&state).unwrap()).unwrap();
    // The invite is sensitive (as an upgraded ingest now says), with no graph change.
    items.invite_sensitive.store(true, Ordering::SeqCst);

    let v = Vault::new(&root, g.clone(), items.clone(), config()).unwrap();
    assert!(v.needs_full_render());
    assert!(g.changes_since(mark, 10).unwrap().0.is_empty());
    let report = v.render_changed(mark).unwrap();
    assert!(!v.needs_full_render());
    assert!(report.removed.len() >= 2, "{report:?}");
    for id in [&jonas, &session] {
        assert_eq!(v.page_path(id).unwrap(), None, "{id}");
    }
    assert_nowhere(
        &root,
        &[
            "Jonas",
            "Counselling",
            "Quay Lane",
            "SECRET",
            "counselling.ics",
        ],
    );
    assert!(v.validate().unwrap().is_ok());
    // Once only: the next sync with no change does nothing.
    let again = v.render_changed(report.high_water).unwrap();
    assert!(
        again.written.is_empty() && again.removed.is_empty(),
        "{again:?}"
    );
}

/// An entity with a sensitive origin shows only the aliases and identifiers that a
/// non-sensitive item supplied; one without shows them all.
#[test]
fn frontmatter_leaves_out_aliases_and_identifiers_learned_from_sensitive_items() {
    let f = fixture();
    let g = &f.graph;
    add_counselling(g, &f.sam);
    let invite = item("files", "counselling.ics");
    let notes = item("files", "notes.md");
    g.add_alias(&f.sam, "Sammy Lee-Albers").unwrap();
    g.add_detail_source(
        &f.sam,
        &EntityDetail::Alias("Sammy Lee-Albers".into()),
        &invite,
    )
    .unwrap();
    g.add_alias(&f.sam, "Sam L").unwrap();
    g.add_detail_source(&f.sam, &EntityDetail::Alias("Sam L".into()), &invite)
        .unwrap();
    g.add_detail_source(&f.sam, &EntityDetail::Alias("Sam L".into()), &notes)
        .unwrap();
    let secret_phone = Identifier::new("phone", "+49 30 1234567");
    g.add_identifier(&f.sam, &secret_phone).unwrap();
    g.add_detail_source(&f.sam, &EntityDetail::Identifier(secret_phone), &invite)
        .unwrap();
    let mail = Identifier::new("email", "sam@example.com");
    g.add_detail_source(&f.sam, &EntityDetail::Identifier(mail), &notes)
        .unwrap();
    let v = Vault::new(
        f.root.join("v2"),
        f.graph.clone(),
        Arc::new(lookup),
        VaultConfig {
            include_identifiers: true,
            ..config()
        },
    )
    .unwrap();
    v.render_all().unwrap();
    let page = std::fs::read_to_string(v.page_path(&f.sam).unwrap().unwrap()).unwrap();
    let fm: Vec<&str> = page
        .lines()
        .take_while(|l| !l.contains("nugget:begin"))
        .collect();
    let fm = fm.join("\n");
    // Supplied by a non-sensitive item (also by the sensitive one): shown.
    assert!(fm.contains("\"Sam L\""), "{fm}");
    assert!(fm.contains("sam@example.com"), "{fm}");
    // Only from the invite, or with no recorded source: left out.
    assert!(!fm.contains("Sammy"), "{fm}");
    assert!(!fm.contains("1234567"), "{fm}");
    assert!(!fm.contains("\"Sam\""), "{fm}");
    assert!(
        !page.contains("Sammy") && !page.contains("1234567"),
        "{page}"
    );
    // Acme has no sensitive origin: everything it has is shown.
    g.add_alias(&f.acme, "Acme Corp").unwrap();
    v.render_all().unwrap();
    let acme = std::fs::read_to_string(v.page_path(&f.acme).unwrap().unwrap()).unwrap();
    assert!(acme.contains("aliases: [\"Acme Corp\"]"), "{acme}");
}

/// A held alias the page does not show, kept in the user's frontmatter, is not added again
/// (which would mark the entity changed, and re-render it, on every pass).
#[test]
fn a_hidden_alias_in_the_users_frontmatter_does_not_churn_the_graph() {
    let f = fixture();
    add_counselling(&f.graph, &f.sam);
    f.graph.add_alias(&f.sam, "Sammy").unwrap();
    f.graph
        .add_detail_source(
            &f.sam,
            &EntityDetail::Alias("Sammy".into()),
            &item("files", "counselling.ics"),
        )
        .unwrap();
    f.vault.render_all().unwrap();
    let path = f.page(&f.sam);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("Sammy"), "{text}");
    let edited = text.replacen("aliases: []", "aliases: [\"Sammy\"]", 1);
    assert_ne!(edited, text, "{text}");
    std::fs::write(&path, edited).unwrap();
    let (_, head) = f.graph.changes_since(0, 10_000).unwrap();
    let rep = f.vault.apply_user_edits(&path).unwrap();
    assert!(rep.aliases_added.is_empty(), "{rep:?}");
    let (changed, _) = f.graph.changes_since(head, 10).unwrap();
    assert!(changed.is_empty(), "{changed:?}");
}

/// Counts lookups: the vault asks in batches and never twice for an item in one pass.
#[derive(Default)]
struct Counting {
    single: std::sync::atomic::AtomicUsize,
    per_item: std::sync::Mutex<std::collections::HashMap<ItemRef, usize>>,
}

impl ItemLookup for Counting {
    fn source_info(&self, it: &ItemRef) -> nugget_core::Result<Option<SourceInfo>> {
        self.single
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(lookup(it))
    }

    fn source_infos(
        &self,
        items: &[ItemRef],
    ) -> nugget_core::Result<std::collections::HashMap<ItemRef, SourceInfo>> {
        let mut seen = self.per_item.lock().unwrap();
        let mut out = std::collections::HashMap::new();
        for it in items {
            *seen.entry(it.clone()).or_default() += 1;
            if let Some(i) = lookup(it) {
                out.insert(it.clone(), i);
            }
        }
        Ok(out)
    }

    fn missing_is_sensitive(&self) -> bool {
        true
    }
}

#[test]
fn items_are_looked_up_in_batches_once_per_pass() {
    let dir = tempfile::tempdir().unwrap();
    let g = Arc::new(MemGraph::new());
    let (sam, ..) = populate(&g);
    add_counselling(&g, &sam);
    let items = Arc::new(Counting::default());
    let v = Vault::new(dir.path().join("v"), g, items.clone(), config()).unwrap();
    v.render_all().unwrap();
    assert_eq!(
        items.single.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no one-item lookups"
    );
    let seen = items.per_item.lock().unwrap();
    assert!(seen.contains_key(&item("files", "counselling.ics")));
    assert!(seen.values().all(|n| *n == 1), "{seen:?}");
}
