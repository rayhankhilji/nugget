import { useEffect, useId, useRef, useState } from "react";
import { api, changeAffects, errorMessage, events, type ClaimStatus, type ClaimView, type EntityDetail, type MentionView } from "../api";
import { useAsync, useEvent, useOpener } from "../hooks";
import {
  attributeRows,
  claimText,
  CLAIM_STATUS_LABEL,
  confidenceTone,
  evidenceSummary,
  formatConfidence,
  formatEvidence,
  groupClaims,
  kindLabel,
  vaultLinks,
} from "../knowledge";
import { MergeDialog } from "./MergeDialog";
import { Badge, ErrorNote, Spinner } from "./ui";

const MENTIONS_SHOWN = 20;

/**
 * One entity: name, kind, aliases, facts grouped by predicate (each with confidence, status and
 * expandable evidence), mentions, and actions (open the vault page, show in graph, merge).
 */
export function EntityDetailPanel({
  id,
  vaultRoot,
  sourceNames,
  onSelect,
  onShowInGraph,
  onMerged,
  onClose,
}: {
  id: string;
  vaultRoot: string | null;
  sourceNames: ReadonlyMap<string, string>;
  /** Open another entity (a fact's object, a merge result). */
  onSelect: (id: string) => void;
  onShowInGraph?: (id: string) => void;
  onMerged: (into: string) => void;
  onClose?: () => void;
}) {
  const detail = useAsync(() => api.knowledgeEntity(id), [id]);
  const [overrides, setOverrides] = useState<Record<number, ClaimStatus>>({});
  const [busyClaim, setBusyClaim] = useState<number | null>(null);
  const [undo, setUndo] = useState<{ claim: ClaimView; text: string } | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [announce, setAnnounce] = useState("");
  const [merging, setMerging] = useState(false);
  const opener = useOpener();
  const headingId = useId();

  // A new entity starts clean.
  useEffect(() => {
    setUndo(null);
    setActionError(null);
    setAnnounce("");
  }, [id]);
  // Fresh data from the daemon replaces optimistic status changes.
  useEffect(() => setOverrides({}), [detail.data]);

  const { reload } = detail;
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  useEvent(events.onKnowledgeChanged, (c) => {
    if (!changeAffects(c, id)) return;
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(reload, 500);
  });

  const setClaimStatus = async (claim: ClaimView, next: "retracted" | "active", prev: ClaimStatus) => {
    setBusyClaim(claim.id);
    setActionError(null);
    setOverrides((o) => ({ ...o, [claim.id]: next }));
    const text = claimText(claim);
    try {
      await api.knowledgeClaimStatus({ claim: claim.id, status: next });
      if (next === "retracted") {
        setUndo({ claim, text });
        setAnnounce(`Rejected “${text}”.`);
      } else {
        setUndo((u) => (u?.claim.id === claim.id ? null : u));
        setAnnounce(`Restored “${text}”.`);
      }
    } catch (e) {
      setOverrides((o) => ({ ...o, [claim.id]: prev }));
      setActionError(`Couldn't ${next === "retracted" ? "reject" : "restore"} the fact: ${errorMessage(e)}`);
    } finally {
      setBusyClaim(null);
    }
  };

  const d = detail.data?.id === id ? detail.data : undefined;

  return (
    <section className="entity-detail" aria-labelledby={headingId} aria-busy={detail.loading || undefined}>
      {!d && (
        <div className="entity-detail-head">
          <h2 id={headingId} className="entity-name">
            {detail.loading ? "Loading…" : "Entity"}
          </h2>
          {onClose && <CloseButton onClose={onClose} />}
        </div>
      )}
      {detail.loading && !d && <Spinner />}
      <ErrorNote error={detail.error} onRetry={detail.reload} />
      {d && (
        <>
          <Header detail={d} headingId={headingId} onClose={onClose} />
          <div className="entity-actions">
            <VaultButtons detail={d} vaultRoot={vaultRoot} open={opener.open} />
            {onShowInGraph && (
              <button type="button" className="btn btn-small" onClick={() => onShowInGraph(d.id)}>
                Show in graph
              </button>
            )}
            <button type="button" className="btn btn-small" onClick={() => setMerging(true)}>
              Merge…
            </button>
          </div>
          <div aria-live="polite" className="entity-live">
            {opener.status && (
              <p className={opener.status.error ? "note note-error small" : "note small"}>{opener.status.text}</p>
            )}
            {undo && (
              <p className="note small undo-note">
                <span>
                  Rejected “{undo.text}”. It won't be shown to apps or written to your vault.
                </span>
                <button
                  type="button"
                  className="btn btn-small"
                  disabled={busyClaim !== null}
                  onClick={() => void setClaimStatus(undo.claim, "active", "retracted")}
                >
                  Undo
                </button>
              </p>
            )}
            <span className="visually-hidden">{announce}</span>
          </div>
          {actionError && (
            <p className="note note-error small" role="alert">
              {actionError}
            </p>
          )}

          <Facts
            detail={d}
            overrides={overrides}
            busyClaim={busyClaim}
            sourceNames={sourceNames}
            onSelect={onSelect}
            onOpen={opener.open}
            onReject={(c, prev) => void setClaimStatus(c, "retracted", prev)}
            onRestore={(c) => void setClaimStatus(c, "active", "retracted")}
          />
          <Mentions mentions={d.mentions} sourceNames={sourceNames} onOpen={opener.open} />
          {merging && (
            <MergeDialog
              target={d}
              onClose={() => setMerging(false)}
              onMerged={(into) => {
                setMerging(false);
                onMerged(into);
                if (into === d.id) reload();
              }}
            />
          )}
        </>
      )}
    </section>
  );
}

function CloseButton({ onClose }: { onClose: () => void }) {
  return (
    <button type="button" className="btn btn-ghost btn-icon" onClick={onClose} aria-label="Close details">
      <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true" focusable="false">
        <path d="M2 2l10 10M12 2L2 12" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
      </svg>
    </button>
  );
}

function Header({ detail: d, headingId, onClose }: { detail: EntityDetail; headingId: string; onClose?: () => void }) {
  const aliases = d.aliases.filter((a) => a !== d.name);
  const attrs = attributeRows(d.attributes);
  return (
    <>
      <div className="entity-detail-head">
        <div>
          <h2 id={headingId} className="entity-name">
            {d.name}
          </h2>
          <p className="entity-sub">
            <Badge tone="accent">{kindLabel(d.kind)}</Badge> <code className="muted small">{d.id}</code>
          </p>
        </div>
        {onClose && <CloseButton onClose={onClose} />}
      </div>
      {(aliases.length > 0 || d.identifiers.length > 0 || attrs.length > 0) && (
        <dl className="entity-props">
          {aliases.length > 0 && (
            <div>
              <dt>Also known as</dt>
              <dd>{aliases.join(", ")}</dd>
            </div>
          )}
          {d.identifiers.map(([scheme, value]) => (
            <div key={`${scheme}\u0000${value}`}>
              <dt>{scheme}</dt>
              <dd>
                <code>{value}</code>
              </dd>
            </div>
          ))}
          {attrs.map(([k, v]) => (
            <div key={k}>
              <dt>{k}</dt>
              <dd>{v}</dd>
            </div>
          ))}
        </dl>
      )}
    </>
  );
}

function VaultButtons({
  detail,
  vaultRoot,
  open,
}: {
  detail: EntityDetail;
  vaultRoot: string | null;
  open: (url: string, label: string) => Promise<void>;
}) {
  const links = vaultLinks(vaultRoot, detail.vault_path);
  if (!links) {
    return (
      <button type="button" className="btn btn-small" disabled title="This entity has no vault page yet">
        Open in vault
      </button>
    );
  }
  const label = `${detail.name}'s vault page`;
  return (
    <>
      <button type="button" className="btn btn-small" onClick={() => void open(links.file, label)}>
        Open in vault
      </button>
      {links.obsidian && (
        <button type="button" className="btn btn-small btn-ghost" onClick={() => void open(links.obsidian!, label)}>
          Open in Obsidian
        </button>
      )}
    </>
  );
}

function Facts({
  detail,
  overrides,
  busyClaim,
  sourceNames,
  onSelect,
  onOpen,
  onReject,
  onRestore,
}: {
  detail: EntityDetail;
  overrides: Record<number, ClaimStatus>;
  busyClaim: number | null;
  sourceNames: ReadonlyMap<string, string>;
  onSelect: (id: string) => void;
  onOpen: (url: string, label: string) => Promise<void>;
  onReject: (c: ClaimView, prev: ClaimStatus) => void;
  onRestore: (c: ClaimView) => void;
}) {
  const claims = detail.claims.map((c) => (overrides[c.id] ? { ...c, status: overrides[c.id]! } : c));
  const groups = groupClaims(claims);
  const headingId = useId();
  return (
    <section aria-labelledby={headingId}>
      <h3 id={headingId} className="entity-section">
        Facts <span className="muted small">({claims.filter((c) => c.status !== "retracted").length})</span>
      </h3>
      {groups.length === 0 && <p className="muted small">No facts about {detail.name} yet.</p>}
      {groups.map((g) => (
        <div key={g.predicate} className="fact-group">
          <h4 className="fact-predicate">{g.label}</h4>
          <ul className="facts">
            {g.claims.map((c) => (
              <Fact
                key={c.id}
                claim={c}
                original={detail.claims.find((x) => x.id === c.id)?.status ?? c.status}
                busy={busyClaim === c.id}
                sourceNames={sourceNames}
                selfId={detail.id}
                onSelect={onSelect}
                onOpen={onOpen}
                onReject={onReject}
                onRestore={onRestore}
              />
            ))}
          </ul>
        </div>
      ))}
    </section>
  );
}

function Fact({
  claim: c,
  original,
  busy,
  sourceNames,
  selfId,
  onSelect,
  onOpen,
  onReject,
  onRestore,
}: {
  claim: ClaimView;
  /** Status before any optimistic change, so a failed reject restores it. */
  original: ClaimStatus;
  busy: boolean;
  sourceNames: ReadonlyMap<string, string>;
  selfId: string;
  onSelect: (id: string) => void;
  onOpen: (url: string, label: string) => Promise<void>;
  onReject: (c: ClaimView, prev: ClaimStatus) => void;
  onRestore: (c: ClaimView) => void;
}) {
  const [open, setOpen] = useState(false);
  const evidenceId = useId();
  const text = claimText(c);
  const retracted = c.status === "retracted";
  const other = c.object.entity && c.object.entity.id !== selfId ? c.object.entity : null;
  return (
    <li className={`fact${retracted ? " fact-retracted" : ""}${c.status === "superseded" ? " fact-superseded" : ""}`}>
      <p className="fact-text">
        {retracted ? <s>{text}</s> : text}
        {c.status === "user_asserted" && (
          <span className="fact-you" title="You added or edited this fact in your vault">
            {" "}
            (you)
          </span>
        )}
      </p>
      <div className="fact-meta">
        <Badge tone={confidenceTone(c.confidence)}>
          <span className="visually-hidden">Confidence </span>
          {formatConfidence(c.confidence)}
        </Badge>
        {c.status !== "active" && c.status !== "user_asserted" && (
          <Badge tone={retracted ? "bad" : "neutral"}>{CLAIM_STATUS_LABEL[c.status] ?? c.status}</Badge>
        )}
        {other && (
          <button type="button" className="btn btn-ghost btn-small" onClick={() => onSelect(other.id)}>
            {other.name} →
          </button>
        )}
        {c.evidence.length > 0 ? (
          <button type="button" className="btn btn-ghost btn-small" aria-expanded={open} aria-controls={evidenceId} onClick={() => setOpen((o) => !o)}>
            Why? <span className="muted">{evidenceSummary(c.evidence)}</span>
          </button>
        ) : (
          <span className="muted small">{c.status === "user_asserted" ? "From your vault edit" : "No sources"}</span>
        )}
        <button
          type="button"
          className={retracted ? "btn btn-small" : "btn btn-small btn-danger-ghost"}
          disabled={busy}
          aria-label={retracted ? `Restore this fact: ${text}` : `Reject this fact: ${text}`}
          onClick={() => (retracted ? onRestore(c) : onReject(c, original))}
        >
          {busy ? <Spinner label="Saving" /> : retracted ? "Restore" : "Reject this fact"}
        </button>
      </div>
      <div id={evidenceId} hidden={!open}>
        {open && (
          <ul className="evidence">
            {c.evidence.map((ev, i) => {
              const e = formatEvidence(ev, sourceNames);
              return (
                <li key={`${ev.item.source}\u0000${ev.item.key}\u0000${ev.chunk_id ?? i}`}>
                  {e.quote ? (
                    <blockquote className="evidence-quote" title={e.truncated ? ev.quote ?? undefined : undefined}>
                      {e.quote}
                    </blockquote>
                  ) : (
                    <p className="muted small">No quote recorded for this source.</p>
                  )}
                  <div className="evidence-source">
                    <span className="evidence-title">{e.title}</span>
                    <span className="muted small">{e.source}</span>
                    {e.openUrl && (
                      <button type="button" className="btn btn-small" onClick={() => void onOpen(e.openUrl!, e.title)}>
                        Open
                      </button>
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </li>
  );
}

function Mentions({
  mentions,
  sourceNames,
  onOpen,
}: {
  mentions: MentionView[];
  sourceNames: ReadonlyMap<string, string>;
  onOpen: (url: string, label: string) => Promise<void>;
}) {
  const [all, setAll] = useState(false);
  const headingId = useId();
  const shown = all ? mentions : mentions.slice(0, MENTIONS_SHOWN);
  return (
    <section aria-labelledby={headingId}>
      <h3 id={headingId} className="entity-section">
        Mentions <span className="muted small">({mentions.length.toLocaleString()})</span>
      </h3>
      {mentions.length === 0 && <p className="muted small">Not mentioned in any indexed item yet.</p>}
      <ul className="mentions">
        {shown.map((m, i) => {
          const e = formatEvidence({ item: m.item, chunk_id: m.chunk_id, title: m.title, url: m.url, quote: null }, sourceNames);
          return (
            <li key={`${m.item.source}\u0000${m.item.key}\u0000${m.chunk_id ?? ""}\u0000${i}`}>
              <span className="mention-surface">“{m.surface}”</span>
              <span className="mention-title">{e.title}</span>
              <span className="muted small">{e.source}</span>
              {e.openUrl && (
                <button type="button" className="btn btn-small btn-ghost" onClick={() => void onOpen(e.openUrl!, e.title)}>
                  Open
                </button>
              )}
            </li>
          );
        })}
      </ul>
      {mentions.length > MENTIONS_SHOWN && (
        <button type="button" className="btn btn-ghost btn-small" onClick={() => setAll((a) => !a)} aria-expanded={all}>
          {all ? "Show fewer" : `Show all ${mentions.length.toLocaleString()} mentions`}
        </button>
      )}
    </section>
  );
}
