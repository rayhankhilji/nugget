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
    // A corrected fact was never true: it is not shown as history either.
    assert!(!t.contains("oat milk") && !t.contains("*Earlier:*"), "{t}");
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

/// The lines inside a generated block (without its markers), each ending in a newline.
fn block_body(text: &str, name: &str) -> String {
    let b = format!("<!-- nugget:begin {name} -->\n");
    let e = format!("<!-- nugget:end {name} -->");
    let s = text.find(&b).unwrap() + b.len();
    text[s..text.find(&e).unwrap()].to_owned()
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
    // The sentence named the old name: it is re-generated with the new one (GG5-10).
    assert!(
        sam.contains(
            "- Works at [[Organization/Acme Corp (ent_000002)|Acme Corp]] [^2] <!-- c:1 -->"
        ),
        "{sam}"
    );
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
    // A PII hit is confirmed sensitive (`nugget_retrieve::is_confirmed_sensitive`): never cited.
    let mut note = nugget_store::NewItem::new("files", "notes.md", "h2");
    note.title = Some("Notes".into());
    note.metadata = serde_json::json!({"sensitive": true, "pii": {"card": 1}});
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

/// With no local model every item waits for triage (`sensitive: true`, triage pending or
/// unknown): such unsorted items are shown in the vault like any other. Once triage confirms one
/// sensitive, it leaves the vault at the next full render.
#[test]
fn unsorted_items_are_shown_and_leave_once_triage_confirms_them_sensitive() {
    let store = nugget_store::Store::open_in_memory().unwrap();
    let unsorted = |key: &str, hash: &str, state: &str| {
        let mut it =
            nugget_store::NewItem::new(if key == "msg-1" { "mail" } else { "files" }, key, hash);
        it.title = Some(key.into());
        it.metadata = serde_json::json!({"sensitive": true, "triage": {"state": state}});
        it
    };
    store
        .upsert_item(&unsorted("msg-1", "h1", "pending"))
        .unwrap();
    store
        .upsert_item(&unsorted("notes.md", "h2", "unknown"))
        .unwrap();
    let g = Arc::new(MemGraph::new());
    let (sam, ..) = populate(&g);
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::new(dir.path(), g, Arc::new(store.clone()), config()).unwrap();
    v.render_all().unwrap();
    let page = v
        .page_path(&sam)
        .unwrap()
        .expect("unsorted items keep the page");
    let t = std::fs::read_to_string(&page).unwrap();
    assert!(
        t.contains("Sam from Acme here") && t.contains("oat milk please"),
        "{t}"
    );

    // Triage runs: both items are sensitive.
    for (item, hash) in [(("mail", "msg-1"), "h1"), (("files", "notes.md"), "h2")] {
        let r = nugget_core::ids::ItemRef {
            source: nugget_core::ids::SourceId::new(item.0),
            key: nugget_core::ids::ItemKey::new(item.1),
        };
        let meta = serde_json::json!({
            "sensitive": true,
            "triage": {"state": "done", "sensitive": true, "junk": false}
        });
        assert!(store.set_item_metadata(&r, hash, &meta).unwrap());
    }
    v.render_all().unwrap();
    assert_eq!(
        v.page_path(&sam).unwrap(),
        None,
        "the page of a now sensitive-only entity goes"
    );
    assert!(!page.exists(), "nothing written on it: deleted");
    assert_nowhere(v.root(), &["Sam from Acme here", "oat milk please"]);
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

// ---- history and merge log ----

fn day(y: i32, m: u8, d: u8) -> time::OffsetDateTime {
    time::Date::from_calendar_date(y, time::Month::try_from(m).unwrap(), d)
        .unwrap()
        .midnight()
        .assume_utc()
}

/// Sam used to work at Initech (2025-01-15 – 2026-09-30) and lived in Boston (no dates); both
/// are superseded. Returns `(initech, worked_at, lived_in)`.
fn add_history(f: &Fixture) -> (EntityId, ClaimId, ClaimId) {
    let initech = f
        .graph
        .create_entity(&NewEntity::new(EntityKind::Organization, "Initech"))
        .unwrap();
    let mut old = claim(
        &f.sam,
        "works_at",
        ClaimObject::Entity(initech.clone()),
        "Samantha works at Initech.",
        vec![ev("mail", "msg-0", "Sam (Initech)")],
    );
    old.valid_from = Some(day(2025, 1, 15));
    old.valid_to = Some(day(2026, 9, 30));
    let worked_at = f.graph.add_claim(&old).unwrap();
    f.graph
        .set_claim_status(worked_at, ClaimStatus::Superseded)
        .unwrap();
    let lived = f
        .graph
        .add_claim(&claim(
            &f.sam,
            "lives_in",
            ClaimObject::Literal("Boston".into()),
            "Lives in Boston.",
            vec![ev("files", "notes.md", "Sam moved from Boston")],
        ))
        .unwrap();
    f.graph
        .set_claim_status(lived, ClaimStatus::Superseded)
        .unwrap();
    (initech, worked_at, lived)
}

#[test]
fn superseded_claims_render_as_cited_history() {
    let f = fixture();
    let (initech, worked_at, lived) = add_history(&f);
    // Rejected claims never show up, current or history.
    let rejected = f
        .graph
        .add_claim(&claim(
            &f.sam,
            "works_at",
            ClaimObject::Literal("Globex".into()),
            "Samantha works at Globex.",
            vec![ev("mail", "msg-1", "Globex")],
        ))
        .unwrap();
    f.graph
        .set_claim_status(rejected, ClaimStatus::Rejected)
        .unwrap();
    f.vault.render_all().unwrap();
    let t = f.read(&f.sam);
    let facts = block_body(&t, "facts");
    assert_eq!(
        facts,
        format!(
            "## Facts\n\n\
### Lives in <!-- p:lives_in -->\n\
*Earlier:*\n  - Lives in Boston. [^1] <!-- h:{lived} -->\n\n\
### Prefers <!-- p:prefers -->\n\
- Prefers oat milk in coffee. [^2][^3] <!-- c:2 -->\n\n\
### Works at <!-- p:works_at -->\n\
- Samantha works at [[Organization/Acme (ent_000002)|Acme]]. [^3] <!-- c:1 -->\n\n\
*Earlier:*\n  - Samantha works at [[Organization/Initech ({})|Initech]]. (2025-01-15 – 2026-09-30) [^4] <!-- h:{worked_at} -->\n",
            short_id(&initech)
        ),
        "{t}"
    );
    assert!(!t.contains("Globex"));
    assert!(t.contains("[^4]: [msg-0](nugget://item/mail/msg-0): \"Sam (Initech)\""));
    // The history link resolves: Initech got a page.
    assert!(f.vault.page_path(&initech).unwrap().is_some());
    let report = f.vault.validate().unwrap();
    assert!(report.is_ok(), "{report:?}");
    assert_eq!((report.facts, report.history_facts), (4, 2));
    assert_eq!(report.cited_facts, 4);
    // Deterministic: re-rendering writes nothing.
    assert!(f.vault.render_all().unwrap().written.is_empty());
}

#[test]
fn history_order_and_intervals() {
    let f = fixture();
    let mk = |text: &str, from: Option<time::OffsetDateTime>, to: Option<time::OffsetDateTime>| {
        let mut c = claim(
            &f.sam,
            "has_role",
            ClaimObject::Literal(text.into()),
            text,
            vec![ev("mail", "msg-1", "Sam from Acme here")],
        );
        c.valid_from = from;
        c.valid_to = to;
        f.graph.add_claim(&c).unwrap()
    };
    let a = mk("Role A.", None, Some(day(2026, 10, 5)));
    let b = mk("Role B.", Some(day(2026, 9, 1)), Some(day(2026, 10, 5)));
    let c = mk("Role C.", Some(day(2024, 3, 1)), None);
    let d = mk("Role D.", None, None);
    for id in [a, b, c, d] {
        f.graph
            .set_claim_status(id, ClaimStatus::Superseded)
            .unwrap();
    }
    // Current facts: newest validity start first, unknown last.
    let cur_old = mk("Lead.", Some(day(2025, 1, 1)), None);
    let cur_new = mk("Director.", Some(day(2026, 10, 6)), None);
    let cur_unk = mk("Mentor.", None, None);
    f.vault.render_all().unwrap();
    let t = f.read(&f.sam);
    let want = format!(
        "### Has role <!-- p:has_role -->\n\
- Director. [^1] <!-- c:{cur_new} -->\n\
- Lead. [^1] <!-- c:{cur_old} -->\n\
- Mentor. [^1] <!-- c:{cur_unk} -->\n\n\
*Earlier:*\n  - Role A. (until 2026-10-05) [^1] <!-- h:{a} -->\n  - Role B. (2026-09-01 – 2026-10-05) [^1] <!-- h:{b} -->\n  - Role C. (from 2024-03-01) [^1] <!-- h:{c} -->\n  - Role D. [^1] <!-- h:{d} -->\n"
    );
    assert!(t.contains(&want), "{t}");
    assert!(f.vault.validate().unwrap().is_ok());
}

#[test]
fn editing_or_deleting_history_changes_nothing() {
    let f = fixture();
    let (_, worked_at, lived) = add_history(&f);
    f.vault.render_all().unwrap();
    let before = f.read(&f.sam);
    f.edit(&f.sam, |t| {
        t.lines()
            .filter(|l| !l.contains(&format!("<!-- h:{lived} -->")))
            .map(|l| {
                if l.contains(&format!("<!-- h:{worked_at} -->")) {
                    format!("- Samantha never worked at Initech. <!-- h:{worked_at} -->")
                } else {
                    l.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    });
    let rep = f.vault.apply_user_edits(f.page(&f.sam)).unwrap();
    assert!(rep.is_empty(), "{rep:?}");
    assert_eq!(f.status(worked_at), ClaimStatus::Superseded);
    assert_eq!(f.status(lived), ClaimStatus::Superseded);
    // History lines are machine-owned: the next render restores them.
    assert_eq!(f.read(&f.sam), before);
    assert!(f.vault.validate().unwrap().is_ok());
}

#[test]
fn corrected_facts_are_not_history() {
    let f = fixture();
    f.vault.render_all().unwrap();
    // A note under the corrected fact stays in its group.
    f.edit(&f.sam, |t| {
        t.replace(
            "- Prefers oat milk in coffee. [^1][^2] <!-- c:2 -->",
            "- Prefers soy milk in coffee. [^1][^2] <!-- c:2 -->\n  - (since the spring)",
        )
    });
    let rep = f.vault.apply_user_edits(f.page(&f.sam)).unwrap();
    let new = rep.asserted[0];
    let want = format!(
        "### Prefers <!-- p:prefers -->\n- Prefers soy milk in coffee. (you) <!-- c:{new} -->\n  - (since the spring)\n\n### Works at"
    );
    let t = f.read(&f.sam);
    assert!(t.contains(&want), "{t}");
    let report = f.vault.validate().unwrap();
    assert!(report.is_ok(), "{report:?}");
    assert_eq!(report.history_facts, 0);
    // The manifest records the correction; without it, the rule still holds: a superseded claim
    // with no end date and a later user assertion in its group is a correction.
    std::fs::remove_file(f.root.join(".nugget/state.json")).unwrap();
    let v2 = Vault::new(&f.root, f.graph.clone(), Arc::new(lookup), config()).unwrap();
    v2.render_all().unwrap();
    let t = std::fs::read_to_string(v2.page_path(&f.sam).unwrap().unwrap()).unwrap();
    assert!(t.contains(&want) && !t.contains("oat milk"), "{t}");
    // Later supersessions in the same group are history again: they carry an end date, or
    // happened after the user's assertion.
    let mut later = claim(
        &f.sam,
        "prefers",
        ClaimObject::Literal("tea".into()),
        "Prefers tea.",
        vec![ev("mail", "msg-1", "Sam from Acme here")],
    );
    later.valid_to = Some(day(2026, 10, 20));
    let later = f.graph.add_claim(&later).unwrap();
    f.graph
        .set_claim_status(later, ClaimStatus::Superseded)
        .unwrap();
    v2.render_all().unwrap();
    let t = std::fs::read_to_string(v2.page_path(&f.sam).unwrap().unwrap()).unwrap();
    assert!(
        t.contains(&format!(
            "*Earlier:*\n  - Prefers tea. (until 2026-10-20) [^1] <!-- h:{later} -->\n"
        )),
        "{t}"
    );
    assert!(!t.contains("oat milk"));
}

fn set_merge_log(f: &Fixture, log: serde_json::Value) {
    f.graph
        .update_entity(
            &f.sam,
            &EntityPatch {
                attributes: Some(serde_json::json!({ MERGE_LOG_ATTR: log })),
                ..Default::default()
            },
        )
        .unwrap();
}

#[test]
fn merge_log_renders_a_cited_section() {
    let f = fixture();
    set_merge_log(
        &f,
        serde_json::json!([
            {"from": "ent_9", "name": "Sam L", "reasons": ["shared_email", "nickname"],
             "detail": "Same address sam@example.com as Samantha Lee.",
             "item": {"source": "mail", "key": "msg-1"}, "at": "2026-10-01T00:00:00Z"},
            {"name": "S. <!-- nugget:end merged --> [[x]]\n## Notes", "reasons": [],
             "detail": "Hostile\n<!-- c:1 --> detail [^1]"}
        ]),
    );
    f.vault.render_all().unwrap();
    let t = f.read(&f.sam);
    assert_eq!(
        block_body(&t, "merged"),
        "## Merged\n\n\
Nugget recognised these as the same person and combined them into this page:\n\n\
- Sam L: Same address sam@example.com as Samantha Lee. (because: shared email, nickname) [^3]\n\
- S. &lt;!-- nugget:end merged --&gt; [ [x] ] ## Notes: Hostile &lt;!-- c:1 --&gt; detail [ ^1]\n\n\
If any of these is a different person, you can split it off again in Nugget; the facts it brought will move to a page of its own. This section is generated: edits to it are not applied, so keep your own notes below.\n",
        "{t}"
    );
    // The section sits between Related and the sources; its footnote is defined (no quote).
    let related = t.find("<!-- nugget:end related -->").unwrap();
    let merged = t.find("<!-- nugget:begin merged -->").unwrap();
    let sources = t.find("<!-- nugget:begin sources -->").unwrap();
    assert!(related < merged && merged < sources);
    assert!(t.contains("[^3]: [Lunch on Friday](https://mail.example/msg-1)\n"));
    assert_eq!(t.matches("<!-- nugget:end merged -->").count(), 1);
    let report = f.vault.validate().unwrap();
    assert!(report.is_ok(), "{report:?}");
    assert!(f.vault.render_all().unwrap().written.is_empty());
    // Split away again: the section disappears.
    set_merge_log(&f, serde_json::json!([]));
    f.vault.render_all().unwrap();
    let t = f.read(&f.sam);
    assert!(!t.contains("nugget:begin merged"));
    assert_eq!(
        mask_updated(&t),
        SAM_GOLDEN,
        "without history or merges a page renders as before"
    );
}

#[test]
fn merge_log_drops_citations_of_deleted_items_and_escapes_links() {
    let f = fixture();
    set_merge_log(
        &f,
        serde_json::json!([
            {"name": "Sam [L](https://evil.test)", "reasons": ["same_item"],
             "detail": "See [here](javascript:alert(1)).", "item": {"source": "mail", "key": "gone"}}
        ]),
    );
    f.vault.render_all().unwrap();
    let t = f.read(&f.sam);
    // `gone` is unknown to the store: the record stays, its footnote does not.
    assert!(
        t.contains(
            "- Sam [L] (https://evil.test): See [here] (javascript:alert(1)). (because: same item)\n"
        ),
        "{t}"
    );
    assert!(!t.contains("gone"), "{t}");
    assert!(!t.contains("[^3]"), "{t}");
    assert!(f.vault.validate().unwrap().is_ok());
}

#[test]
fn merge_log_shows_the_newest_fifty() {
    let f = fixture();
    let log: Vec<serde_json::Value> = (0..53)
        .map(|i| serde_json::json!({"name": format!("Name {i}")}))
        .collect();
    set_merge_log(&f, serde_json::Value::Array(log));
    f.vault.render_all().unwrap();
    let body = block_body(&f.read(&f.sam), "merged");
    let lines: Vec<&str> = body.lines().filter(|l| l.starts_with("- ")).collect();
    assert_eq!(lines.len(), 51);
    assert_eq!(lines[0], "- … and 3 earlier");
    assert_eq!(lines[1], "- Name 3");
    assert_eq!(lines[50], "- Name 52");
}

#[test]
fn history_marker_edits_are_restored_and_foreign_markers_kept() {
    let f = fixture();
    let (_, worked_at, _) = add_history(&f);
    f.vault.render_all().unwrap();
    let before = f.read(&f.sam);
    // Text appended after a history marker: the line is still history and gets restored.
    f.edit(&f.sam, |t| {
        t.replace(
            &format!("<!-- h:{worked_at} -->"),
            &format!("<!-- h:{worked_at} --> (not sure)"),
        )
    });
    assert!(f.vault.apply_user_edits(f.page(&f.sam)).unwrap().is_empty());
    assert_eq!(f.read(&f.sam), before);
    assert!(f.vault.validate().unwrap().is_ok());
    // A line with a marker the render never showed is the user's text: kept, not applied.
    f.edit(&f.sam, |t| {
        t.replace(
            "<!-- c:1 -->\n",
            "<!-- c:1 -->\n- copied from elsewhere [^1] <!-- h:999 -->\n",
        )
    });
    assert!(f.vault.apply_user_edits(f.page(&f.sam)).unwrap().is_empty());
    assert!(
        f.read(&f.sam)
            .contains("- copied from elsewhere [^1] <!-- h:999 -->\n")
    );
}

#[test]
fn lost_manifest_with_old_pages_upgrades_on_render_changed() {
    let f = fixture();
    f.vault.render_all().unwrap();
    let high = f.graph.changes_since(0, 100_000).unwrap().1;
    // An unreadable manifest: the vault starts from an empty state, but old pages and merge
    // bases are on disk.
    std::fs::write(f.root.join(".nugget/state.json"), "{not json").unwrap();
    let v3 = Vault::new(&f.root, f.graph.clone(), Arc::new(lookup), config()).unwrap();
    // Even with nothing changed in the graph, render_changed does a full pass and records it.
    let r = v3.render_changed(high).unwrap();
    assert_eq!(state_version(&f.root), 2);
    assert!(r.conflicts.is_empty(), "{r:?}");
    assert_eq!(v3.status().pages, 2);
    assert!(v3.validate().unwrap().is_ok());
}

#[test]
fn validator_checks_history_and_merge_citations() {
    let ok = |_: &str| true;
    let page = "---\nnugget_id: \"e\"\nkind: \"person\"\naliases: []\nupdated: \"2026-10-01T00:00:00Z\"\n---\n\
<!-- nugget:begin facts -->\n## Facts\n\n### A <!-- p:a -->\n*Earlier:*\n  - Old. <!-- h:3 -->\n  - Older. [^7] <!-- h:4 -->\n  - Fine. [^1] <!-- h:5 -->\n  - Broken. [^1] <!-- h:x -->\n<!-- nugget:end facts -->\n\
<!-- nugget:begin merged -->\n- M [^8]\n<!-- nugget:end merged -->\n\
<!-- nugget:begin sources -->\n[^1]: [S](nugget://item/a/b)\n<!-- nugget:end sources -->\n";
    let r = validate_page("Person/E.md", page, &ok);
    let msgs: Vec<(IssueKind, &str)> = r
        .issues
        .iter()
        .map(|i| (i.kind, i.message.as_str()))
        .collect();
    assert_eq!(
        msgs,
        [
            (IssueKind::MissingCitation, "history line has no citation"),
            (IssueKind::MissingCitation, "footnote [^7] is not defined"),
            (IssueKind::BadMarker, "claim marker does not parse"),
            (IssueKind::MissingCitation, "footnote [^8] is not defined"),
        ]
    );
    assert_eq!((r.facts, r.history_facts, r.cited_facts), (3, 3, 1));
}

fn state_version(root: &Path) -> serde_json::Value {
    let p = root.join(".nugget/state.json");
    serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(p).unwrap()).unwrap()
        ["version"]
        .clone()
}

fn set_state_version(root: &Path, v: u32) {
    let p = root.join(".nugget/state.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    state["version"] = v.into();
    std::fs::write(&p, serde_json::to_string(&state).unwrap()).unwrap();
}

#[test]
fn old_layout_vault_upgrades_and_keeps_user_edits() {
    let f = fixture();
    f.vault.render_all().unwrap();
    assert_eq!(state_version(&f.root), 2);
    // Turn the vault into one written by the previous version: the page and merge base exactly
    // as the layout-1 renderer wrote them, and a layout-1 manifest.
    let old = SAM_GOLDEN.replace("\"<t>\"", "\"2026-10-01T09:00:00Z\"");
    let base = f
        .root
        .join(format!(".nugget/base/entities/{}.md", short_id(&f.sam)));
    std::fs::write(&base, &old).unwrap();
    set_state_version(&f.root, 1);
    // The user's edits: a paragraph after the frontmatter, a note inside the facts, text in the
    // Notes section and a `## My notes` section at the end.
    let para = "Met Sam at the 2024 offsite; she runs procurement.";
    let user =
        old.replace(
            "---\n<!-- nugget:begin header -->",
            &format!("---\n{para}\n\n<!-- nugget:begin header -->"),
        )
        .replace(
            "<!-- c:1 -->\n",
            "<!-- c:1 -->\n  - (she leads the platform team)\n",
        ) + "Call her about the Q3 plan.\n- [ ] send deck\n\n## My notes\n\nPrefers mornings.\n";
    f.write(&f.sam, &user);
    let mark = f.graph.changes_since(0, 100_000).unwrap().1;

    // Reopen as the new version; the graph meanwhile learnt a job change and a merge.
    let v2 = Vault::new(&f.root, f.graph.clone(), Arc::new(lookup), config()).unwrap();
    let globex = f
        .graph
        .create_entity(&NewEntity::new(EntityKind::Organization, "Globex"))
        .unwrap();
    let mut now = claim(
        &f.sam,
        "works_at",
        ClaimObject::Entity(globex.clone()),
        "Samantha works at Globex.",
        vec![ev("mail", "msg-2", "Sam, now at Globex")],
    );
    now.valid_from = Some(day(2026, 10, 1));
    let now = f.graph.add_claim(&now).unwrap();
    f.graph
        .set_claim_status(f.works_at, ClaimStatus::Superseded)
        .unwrap();
    set_merge_log(
        &f,
        serde_json::json!([{"name": "Sam L", "reasons": ["shared_email"],
            "detail": "Same address.", "item": {"source": "mail", "key": "msg-2"}}]),
    );
    let r = v2.render_changed(mark).unwrap();
    assert!(r.edits.is_empty(), "{r:?}");
    assert!(r.conflicts.is_empty(), "{r:?}");
    let t = std::fs::read_to_string(v2.page_path(&f.sam).unwrap().unwrap()).unwrap();
    assert!(
        t.contains(&format!("---\n{para}\n\n<!-- nugget:begin header -->")),
        "{t}"
    );
    assert!(
        t.ends_with(
            "## Notes\n\nCall her about the Q3 plan.\n- [ ] send deck\n\n## My notes\n\nPrefers mornings.\n"
        ),
        "{t}"
    );
    // The note followed its fact, which is now history.
    assert!(
        t.contains(&format!(
            "### Works at <!-- p:works_at -->\n\
- Samantha works at [[Organization/Globex ({})|Globex]]. [^3] <!-- c:{now} -->\n\n\
*Earlier:*\n  - Samantha works at [[Organization/Acme (ent_000002)|Acme]]. [^2] <!-- h:{} -->\n  - (she leads the platform team)\n",
            short_id(&globex),
            f.works_at
        )),
        "{t}"
    );
    assert!(t.contains("<!-- nugget:end related -->\n\n<!-- nugget:begin merged -->\n## Merged\n"));
    let report = v2.validate().unwrap();
    assert!(report.is_ok(), "{report:?}");
    assert_eq!(report.history_facts, 1);
    assert_eq!(state_version(&f.root), 2);
    // Upgraded once: an unchanged graph renders nothing new.
    let high = f.graph.changes_since(0, 100_000).unwrap().1;
    let again = v2.render_changed(high).unwrap();
    assert!(again.written.is_empty(), "{again:?}");
}

#[test]
fn partial_passes_do_not_mark_an_old_vault_upgraded() {
    let f = fixture();
    f.vault.render_all().unwrap();
    set_state_version(&f.root, 1);
    let v2 = Vault::new(&f.root, f.graph.clone(), Arc::new(lookup), config()).unwrap();
    v2.apply_user_edits(v2.page_path(&f.sam).unwrap().unwrap())
        .unwrap();
    assert_eq!(state_version(&f.root), 1);
    let high = f.graph.changes_since(0, 100_000).unwrap().1;
    v2.render_changed(high).unwrap();
    assert_eq!(state_version(&f.root), 2);
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

    // An LLM rewording its quote on a retry of the same, unchanged item: still replaced.
    assert_eq!(works_at_again(&f, "Sam (Acme) here"), f.works_at);
    assert_eq!(f.status(f.works_at), ClaimStatus::Superseded);

    // Another source states it: normal rules again (a changed item with a new quote is
    // covered by the store tests: `MemGraph` keeps no item versions).
    f.graph
        .add_evidence(f.works_at, &ev("mail", "msg-9", "Sam, Acme"))
        .unwrap();
    assert_eq!(f.status(f.works_at), ClaimStatus::Active);
    f.vault.render_all().unwrap();
    assert!(f.read(&f.sam).contains("Samantha works at"));
}

/// Review findings 6 and 7: the user edits the same line twice before a render, then deletes
/// their own version: the original is free again and the next extraction brings it back.
#[test]
fn second_edit_and_deleting_the_users_version() {
    let f = fixture();
    f.vault.render_all().unwrap();
    f.edit(&f.sam, |t| {
        replace_line(
            &t,
            "<!-- c:1 -->",
            "- Samantha is an Acme engineer <!-- c:1 -->",
        )
    });
    let r = f.vault.render_all().unwrap();
    let user = r.edits.asserted[0];
    // A second edit, of the user's own line.
    assert!(f.read(&f.sam).contains(&format!("<!-- c:{user} -->")));
    f.edit(&f.sam, |t| {
        replace_line(
            &t,
            &format!("<!-- c:{user} -->"),
            &format!("- Samantha is Acme's CTO <!-- c:{user} -->"),
        )
    });
    let r = f.vault.render_all().unwrap();
    assert_eq!(r.edits.superseded, vec![user]);
    let second = r.edits.asserted[0];
    assert!(f.read(&f.sam).contains("Samantha is Acme's CTO"));
    // The user deletes their line.
    f.edit(&f.sam, |t| {
        t.lines()
            .filter(|l| !l.contains(&format!("<!-- c:{second} -->")))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    });
    f.vault.render_all().unwrap();
    assert_eq!(f.status(second), ClaimStatus::Rejected);
    // Re-extraction: the original fact (its replacement is gone) is current again.
    f.graph.retract_item(&item("mail", "msg-1")).unwrap();
    f.graph
        .add_evidence(f.works_at, &ev("mail", "msg-1", "Sam from Acme here"))
        .unwrap();
    assert_eq!(f.status(f.works_at), ClaimStatus::Active);
    f.vault.render_all().unwrap();
    assert!(
        f.read(&f.sam).contains("Samantha works at"),
        "{}",
        f.read(&f.sam)
    );
}

/// Review finding 1: a comment-stripped page that was emptied or rewritten rejects nothing,
/// and a line removed outright from one rejects only that fact.
#[test]
fn stripped_pages_reject_only_clear_deletions() {
    let strip = |t: &str| {
        let mut s = String::new();
        let mut rest = t;
        while let Some(i) = rest.find("<!--") {
            let j = rest[i..].find("-->").unwrap() + i + 3;
            s.push_str(rest[..i].strip_suffix(' ').unwrap_or(&rest[..i]));
            rest = &rest[j..];
        }
        s.push_str(rest);
        s
    };
    for text in [
        "",
        "# Samantha\n\nRewritten from scratch in another editor.\n",
    ] {
        let f = fixture();
        f.vault.render_all().unwrap();
        f.write(&f.sam, text);
        let r = f.vault.render_all().unwrap();
        assert!(r.edits.retracted.is_empty(), "{text:?}: {:?}", r.edits);
        assert_eq!(f.status(f.works_at), ClaimStatus::Active);
        assert_eq!(f.status(f.prefers), ClaimStatus::Active);
        // Second review, finding 6: the file is left exactly as the user has it (no merge that
        // would duplicate it) and reported as skipped.
        assert_eq!(f.read(&f.sam), text);
        assert!(
            r.skipped.iter().any(|p| p.contains("Samantha Lee")),
            "{:?}",
            r.skipped
        );
    }
    let f = fixture();
    f.vault.render_all().unwrap();
    f.edit(&f.sam, |t| {
        strip(&t)
            .lines()
            .filter(|l| !l.contains("oat milk"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    });
    let r = f.vault.render_all().unwrap();
    assert_eq!(r.edits.retracted, vec![f.prefers]);
    assert_eq!(f.status(f.works_at), ClaimStatus::Active);
    assert_eq!(f.read(&f.sam).matches("# Samantha Lee").count(), 1);
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

/// Aliases and identifiers whose recorded sources are all sensitive are left out of the
/// frontmatter; those with a non-sensitive source, or none recorded, are shown.
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
    // No recorded source (the user's own, or from before sources were kept): shown.
    assert!(fm.contains("\"Sam\""), "{fm}");
    // Only from the invite: left out.
    assert!(!fm.contains("Sammy"), "{fm}");
    assert!(!fm.contains("1234567"), "{fm}");
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
    // "sam." is the held alias "Sam" for the graph (normalize_name).
    let edited = text.replacen(
        "aliases: [\"Sam\"]",
        "aliases: [\"Sam\", \"Sammy\", \"sam.\"]",
        1,
    );
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

// ---- hidden pages: user edits, frontmatter, markers, missing base, lookups (GG5-3 review 2) ----

/// A vault with Sam, Jonas and the counselling session rendered while the invite is not yet
/// sensitive. Returns (dir, graph, items, vault, sam, jonas, session).
fn hidden_fixture() -> (
    tempfile::TempDir,
    Arc<MemGraph>,
    Arc<Switch>,
    Vault,
    EntityId,
    EntityId,
    EntityId,
) {
    let dir = tempfile::tempdir().unwrap();
    let g = Arc::new(MemGraph::new());
    let (sam, ..) = populate(&g);
    let (jonas, session, ..) = add_counselling(&g, &sam);
    let items = Arc::new(Switch {
        invite_sensitive: false.into(),
        missing_sensitive: true,
    });
    let v = Vault::new(dir.path().join("v"), g.clone(), items.clone(), config()).unwrap();
    v.render_all().unwrap();
    (dir, g, items, v, sam, jonas, session)
}

fn make_sensitive(items: &Switch) {
    items
        .invite_sensitive
        .store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Frontmatter the user changed counts as their text: the page is kept (hidden), their own
/// keys stay, their alias reaches the graph, and no generated text is left.
#[test]
fn a_hidden_page_keeps_the_users_frontmatter_and_applies_their_alias() {
    let (_dir, g, items, v, _sam, jonas, _) = hidden_fixture();
    let page = v.page_path(&jonas).unwrap().unwrap();
    let text = std::fs::read_to_string(&page).unwrap();
    let edited = text.replacen("aliases: []", "aliases: [\"JA\"]\ntags: [\"therapy\"]", 1);
    assert_ne!(edited, text, "{text}");
    std::fs::write(&page, edited).unwrap();
    make_sensitive(&items);
    let report = v.render_all().unwrap();
    assert!(
        page.is_file(),
        "deleted although the user edited it: {report:?}"
    );
    let stub = std::fs::read_to_string(&page).unwrap();
    assert!(stub.contains("# Hidden page"), "{stub}");
    assert!(stub.contains("tags: [\"therapy\"]"), "{stub}");
    for leak in ["# Jonas", "Organizer", "organized", "SECRET", "Counselling"] {
        assert!(!stub.contains(leak), "{leak} in stub:\n{stub}");
    }
    assert!(
        g.get_entity(&jonas)
            .unwrap()
            .unwrap()
            .aliases
            .contains(&"JA".to_string())
    );
    assert_eq!(v.page_path(&jonas).unwrap(), None);
    assert!(v.validate().unwrap().is_ok());
}

/// A reworded fact line on a page whose entity turns sensitive-only is applied first: the
/// fact is now the user's own, so the entity keeps a normal page.
#[test]
fn a_fact_edit_on_a_page_turning_hidden_is_applied_and_keeps_the_page() {
    let (_dir, g, items, v, _sam, jonas, _) = hidden_fixture();
    let organized = g.claims_for(&jonas, false).unwrap()[0].id;
    let page = v.page_path(&jonas).unwrap().unwrap();
    let text = std::fs::read_to_string(&page).unwrap();
    let marker = format!("<!-- c:{organized} -->");
    let edited = replace_line(
        &text,
        &marker,
        &format!("- Jonas runs a book club. {marker}"),
    );
    std::fs::write(&page, edited).unwrap();
    make_sensitive(&items);
    v.render_all().unwrap();
    let c = g.get_claim(organized).unwrap().unwrap();
    assert!(!c.status.is_active(), "{c:?}");
    assert!(
        g.claims_for(&jonas, false)
            .unwrap()
            .iter()
            .any(|c| c.status == ClaimStatus::UserAsserted && c.text.contains("book club"))
    );
    let now = std::fs::read_to_string(v.page_path(&jonas).unwrap().expect("still a page")).unwrap();
    assert!(now.contains("Jonas runs a book club."), "{now}");
    assert!(
        !now.contains("Organizer") && !now.contains("SECRET"),
        "{now}"
    );
}

/// A deleted fact line on a page turning hidden is applied too (rejected in the graph), and the
/// deleted region text is never kept as notes.
#[test]
fn a_fact_deletion_on_a_page_turning_hidden_is_applied() {
    let (_dir, g, items, v, _sam, jonas, _) = hidden_fixture();
    let organized = g.claims_for(&jonas, false).unwrap()[0].id;
    // A second fact from the invite, so Jonas stays sensitive-only after the deletion.
    g.add_claim(&claim(
        &jonas,
        "phone",
        ClaimObject::Literal("+49 30 555".into()),
        "Jonas's phone is +49 30 555.",
        vec![ev("files", "counselling.ics", "Tel +49 30 555")],
    ))
    .unwrap();
    v.render_all().unwrap();
    let page = v.page_path(&jonas).unwrap().unwrap();
    let text = std::fs::read_to_string(&page).unwrap();
    let marker = format!("<!-- c:{organized} -->");
    let edited: String = text
        .lines()
        .filter(|l| !l.contains(&marker))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&page, format!("{edited}my note\n")).unwrap();
    make_sensitive(&items);
    v.render_all().unwrap();
    assert_eq!(
        g.get_claim(organized).unwrap().unwrap().status,
        ClaimStatus::Rejected
    );
    let stub = std::fs::read_to_string(&page).unwrap();
    assert!(
        stub.contains("my note") && stub.contains("# Hidden page"),
        "{stub}"
    );
    for leak in ["Jonas", "Facts", "Related", "Counselling", "SECRET", "555"] {
        assert!(!stub.contains(leak), "{leak} in stub:\n{stub}");
    }
}

/// A page an editor saved without HTML comments gets its markers back before anything is
/// decided: the hidden stub keeps the note, not the heading or the facts.
#[test]
fn a_comment_stripped_page_turning_hidden_keeps_no_generated_text() {
    let (_dir, _g, items, v, _sam, jonas, _) = hidden_fixture();
    let page = v.page_path(&jonas).unwrap().unwrap();
    let text = std::fs::read_to_string(&page).unwrap();
    let stripped: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("<!-- nugget:"))
        .map(|l| match l.find(" <!--") {
            Some(i) => format!("{}\n", &l[..i]),
            None => format!("{l}\n"),
        })
        .collect();
    assert!(!stripped.contains("<!--"), "{stripped}");
    std::fs::write(&page, format!("{stripped}my note\n")).unwrap();
    make_sensitive(&items);
    v.render_all().unwrap();
    let stub = std::fs::read_to_string(&page).unwrap();
    assert!(stub.contains("my note"), "{stub}");
    for leak in ["# Jonas", "Organizer", "organized", "SECRET", "Counselling"] {
        assert!(!stub.contains(leak), "{leak} in stub:\n{stub}");
    }
}

/// Without a merge base nothing tells the user's text from the generated text: the page is
/// never deleted, it becomes a hidden stub.
#[test]
fn a_page_without_a_merge_base_is_hidden_not_deleted() {
    let (_dir, _g, items, v, _sam, _jonas, session) = hidden_fixture();
    let page = v.page_path(&session).unwrap().unwrap();
    let base = v.root().join(".nugget/base/entities");
    for e in std::fs::read_dir(&base).unwrap() {
        let p = e.unwrap().path();
        if p.to_string_lossy().contains(&short_id(&session)) {
            std::fs::remove_file(p).unwrap();
        }
    }
    make_sensitive(&items);
    let report = v.render_all().unwrap();
    assert!(page.is_file(), "{report:?}");
    assert!(
        !report.removed.iter().any(|r| r.contains("Counselling")),
        "{report:?}"
    );
    let stub = std::fs::read_to_string(&page).unwrap();
    assert!(stub.contains("# Hidden page"), "{stub}");
    assert!(
        !stub.contains("Organizer") && !stub.contains("SECRET"),
        "{stub}"
    );
}

/// A page that cannot be read (here a symlink) does not keep the one-time full render pending.
#[cfg(unix)]
#[test]
fn an_unreadable_page_does_not_force_full_renders_forever() {
    let (_dir, _g, _items, v, sam, ..) = hidden_fixture();
    let root = v.root().to_path_buf();
    let page = v.page_path(&sam).unwrap().unwrap();
    drop(v);
    std::fs::remove_file(&page).unwrap();
    std::os::unix::fs::symlink(root.join("README.md"), &page).unwrap();
    let state_path = root.join(".nugget/state.json");
    let mut state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    state.as_object_mut().unwrap().remove("rules");
    std::fs::write(&state_path, serde_json::to_string(&state).unwrap()).unwrap();
    let g = Arc::new(MemGraph::new());
    populate(&g);
    let v = Vault::new(&root, g, Arc::new(lookup), config()).unwrap();
    assert!(v.needs_full_render());
    let report = v.render_changed(u64::MAX).unwrap();
    assert!(!report.skipped.is_empty(), "{report:?}");
    assert!(!v.needs_full_render(), "{report:?}");
}

/// A lookup whose batch call always fails: the vault asks item by item instead of counting
/// every item as sensitive.
struct NoBatch;

impl ItemLookup for NoBatch {
    fn source_info(&self, it: &ItemRef) -> nugget_core::Result<Option<SourceInfo>> {
        Ok(lookup(it))
    }

    fn source_infos(
        &self,
        _items: &[ItemRef],
    ) -> nugget_core::Result<std::collections::HashMap<ItemRef, SourceInfo>> {
        Err(nugget_core::Error::Invalid("batch broken".into()))
    }
}

#[test]
fn a_failed_batch_lookup_falls_back_to_one_item_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let g = Arc::new(MemGraph::new());
    let (sam, acme, ..) = populate(&g);
    let v = Vault::new(dir.path().join("v"), g, Arc::new(NoBatch), config()).unwrap();
    let report = v.render_all().unwrap();
    assert!(report.removed.is_empty(), "{report:?}");
    let t = std::fs::read_to_string(v.page_path(&sam).unwrap().unwrap()).unwrap();
    assert!(
        t.contains("Samantha works at") && t.contains("Lunch on Friday"),
        "{t}"
    );
    assert!(v.page_path(&acme).unwrap().is_some());
}

/// A lookup that fails for the invite: nothing is deleted in that pass.
struct FailInvite;

impl ItemLookup for FailInvite {
    fn source_info(&self, it: &ItemRef) -> nugget_core::Result<Option<SourceInfo>> {
        if it.key.as_str() == "counselling.ics" {
            return Err(nugget_core::Error::Invalid("store busy".into()));
        }
        Ok(lookup(it))
    }

    fn source_infos(
        &self,
        _items: &[ItemRef],
    ) -> nugget_core::Result<std::collections::HashMap<ItemRef, SourceInfo>> {
        Err(nugget_core::Error::Invalid("batch broken".into()))
    }
}

#[test]
fn no_page_is_deleted_in_a_pass_with_lookup_errors() {
    let (dir, g, _items, v, _sam, _jonas, session) = hidden_fixture();
    let page = v.page_path(&session).unwrap().unwrap();
    let root = v.root().to_path_buf();
    drop(v);
    let v = Vault::new(&root, g, Arc::new(FailInvite), config()).unwrap();
    let report = v.render_all().unwrap();
    assert!(report.removed.is_empty(), "{report:?}");
    assert!(page.is_file());
    // Still hidden: a failed lookup counts as sensitive.
    let stub = std::fs::read_to_string(&page).unwrap();
    assert!(!stub.contains("Organizer"), "{stub}");
    drop(dir);
}
