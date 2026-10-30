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
        }),
        "notes.md" => Some(SourceInfo {
            title: Some("Notes".into()),
            url: None,
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
    // The replaced claim is history now (it is superseded, and its sources still say so).
    assert!(t.contains("*Earlier:*\n  - Prefers oat milk in coffee. [^1][^2] <!-- h:2 -->\n"));
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
    it.metadata = serde_json::json!({"url": "https://mail.example/m?id=1 (x)"});
    store.upsert_item(&it).unwrap();
    let g = Arc::new(MemGraph::new());
    let (sam, ..) = populate(&g);
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::new(dir.path(), g, Arc::new(store), config()).unwrap();
    v.render_all().unwrap();
    let t = std::fs::read_to_string(v.page_path(&sam).unwrap().unwrap()).unwrap();
    assert!(
        t.contains("[^2]: [Quarterly (draft)](https://mail.example/m?id=1%20%28x%29): \"Sam from Acme here\""),
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
*Earlier:*\n  - Role A. (until 2026-10-05) [^1] <!-- h:{a} -->\n  - Role B. (2026-09-01 – 2026-10-05) [^1] <!-- h:{b} -->\n  - Role C. (since 2024-03-01) [^1] <!-- h:{c} -->\n  - Role D. [^1] <!-- h:{d} -->\n"
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
fn edited_fact_moves_to_history() {
    let f = fixture();
    f.vault.render_all().unwrap();
    // A note under the fact follows the claim it was written under (now history).
    f.edit(&f.sam, |t| {
        t.replace(
            "- Prefers oat milk in coffee. [^1][^2] <!-- c:2 -->",
            "- Prefers soy milk in coffee. [^1][^2] <!-- c:2 -->\n  - (since the spring)",
        )
    });
    let rep = f.vault.apply_user_edits(f.page(&f.sam)).unwrap();
    let new = rep.asserted[0];
    let t = f.read(&f.sam);
    assert!(
        t.contains(&format!(
            "### Prefers <!-- p:prefers -->\n- Prefers soy milk in coffee. (you) <!-- c:{new} -->\n\n*Earlier:*\n  - Prefers oat milk in coffee. [^1][^2] <!-- h:2 -->\n  - (since the spring)\n"
        )),
        "{t}"
    );
    let report = f.vault.validate().unwrap();
    assert!(report.is_ok(), "{report:?}");
    assert_eq!(report.history_facts, 1);
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
If any of these is a different person, you can split it off again in Nugget; the facts it brought will move to a page of its own.\n",
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
