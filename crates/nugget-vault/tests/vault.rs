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
