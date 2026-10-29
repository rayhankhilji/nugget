import { useEffect, useId, useMemo, useRef, useState } from "react";
import {
  api,
  changeAffects,
  errorMessage,
  events,
  type ClaimDecision,
  type ClaimStatus,
  type ClaimView,
  type EntityDetail,
  type MentionView,
} from "../api";
import { useAsync, useEvent, useOpener } from "../hooks";
import { Trans, useT } from "../i18n";
import {
  attributeRows,
  claimText,
  claimStatusLabel,
  confidenceTone,
  evidenceSummary,
  factAction,
  formatConfidence,
  formatEvidence,
  groupClaims,
  isCurrentStatus,
  isDroppedStatus,
  kindLabel,
  undoStatus,
  vaultLinks,
  type FactActionPlan,
} from "../knowledge";
import type { MessageKey } from "../i18n";
import { claimTime, evidenceTime, groupClaimsByRecency, hasClaimTimes, sourceKindOf } from "../recency";
import { MergeDialog } from "./MergeDialog";
import { RelativeTime } from "./RelativeTime";
import { SourceIcon } from "./SourceIcon";
import { SplitDialog } from "./SplitDialog";
import { Badge, ErrorNote, Spinner } from "./ui";

const MENTIONS_SHOWN = 20;

/**
 * One entity: name, kind, aliases, facts grouped by recency (or by predicate when the daemon
 * sends no timestamps; each with confidence, status, when it was last seen and expandable
 * evidence), mentions, and actions (open the vault page, show in graph, merge, split).
 */
export function EntityDetailPanel({
  id,
  vaultRoot,
  sourceNames,
  sourceKinds,
  onSelect,
  onShowInGraph,
  onMerged,
  onClose,
}: {
  id: string;
  vaultRoot: string | null;
  sourceNames: ReadonlyMap<string, string>;
  /** Source id → kind, for evidence icons (guessed from the id when missing). */
  sourceKinds?: ReadonlyMap<string, string>;
  /** Open another entity (a fact's object, a merge result). */
  onSelect: (id: string) => void;
  onShowInGraph?: (id: string) => void;
  onMerged: (into: string) => void;
  onClose?: () => void;
}) {
  const t = useT();
  const detail = useAsync(() => api.knowledgeEntity(id), [id]);
  const [overrides, setOverrides] = useState<Record<number, ClaimStatus>>({});
  const [busyClaim, setBusyClaim] = useState<number | null>(null);
  const [undo, setUndo] = useState<{ claim: ClaimView; text: string; plan: FactActionPlan } | null>(null);
  const [splitting, setSplitting] = useState(false);
  const [splitOff, setSplitOff] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [announce, setAnnounce] = useState("");
  const [merging, setMerging] = useState(false);
  const opener = useOpener();
  const headingId = useId();

  // A new entity starts clean.
  useEffect(() => {
    setUndo(null);
    setSplitOff(null);
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

  /** Send a decision with an optimistic status; `false` (and the error shown) if it failed. */
  const send = async (claim: ClaimView, decision: ClaimDecision, next: ClaimStatus, prev: ClaimStatus, failed: MessageKey) => {
    setBusyClaim(claim.id);
    setActionError(null);
    setOverrides((o) => ({ ...o, [claim.id]: next }));
    try {
      await api.knowledgeClaimStatus({ claim: claim.id, status: decision });
      return true;
    } catch (e) {
      setOverrides((o) => ({ ...o, [claim.id]: prev }));
      setActionError(t(failed, { error: errorMessage(e) }));
      return false;
    } finally {
      setBusyClaim(null);
    }
  };

  const act = async (claim: ClaimView, plan: FactActionPlan, prev: ClaimStatus) => {
    const text = claimText(claim);
    const failed: MessageKey =
      plan.action === "reject" ? "entity.rejectFailed" : plan.action === "unassert" ? "entity.unassertFailed" : "entity.restoreFailed";
    if (!(await send(claim, plan.decision, plan.next, prev, failed))) return;
    if (plan.undo) {
      setUndo({ claim, text, plan });
      setAnnounce(t(plan.action === "reject" ? "entity.rejected" : "entity.unasserted", { text }));
    } else {
      setUndo((u) => (u?.claim.id === claim.id ? null : u));
      setAnnounce(t("entity.restored", { text }));
    }
  };

  const undoLast = async () => {
    if (!undo?.plan.undo) return;
    const { claim, text, plan } = undo;
    if (await send(claim, plan.undo!, undoStatus(plan.undo!, claim), plan.next, "entity.restoreFailed")) {
      setUndo(null);
      setAnnounce(t("entity.restored", { text }));
    }
  };

  const d = detail.data?.id === id ? detail.data : undefined;

  return (
    <section className="entity-detail" aria-labelledby={headingId} aria-busy={detail.loading || undefined}>
      {!d && (
        <div className="entity-detail-head">
          <h2 id={headingId} className="entity-name" dir="auto">
            {detail.loading ? t("entity.loading") : t("entity.placeholder")}
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
                {t("entity.showInGraph")}
              </button>
            )}
            <button type="button" className="btn btn-small" onClick={() => setMerging(true)}>
              {t("entity.merge")}
            </button>
            <button type="button" className="btn btn-small" onClick={() => setSplitting(true)} disabled={d.claims.length === 0}>
              {t("entity.split")}
            </button>
          </div>
          <div aria-live="polite" className="entity-live">
            {opener.status && (
              <p className={opener.status.error ? "note note-error small" : "note small"}>{opener.status.text}</p>
            )}
            {undo && (
              <p className="note small undo-note">
                <span>
                  {t(undo.plan.action === "reject" ? "entity.rejectedNote" : "entity.unassertedNote", { text: undo.text })}
                </span>
                <button type="button" className="btn btn-small" disabled={busyClaim !== null} onClick={() => void undoLast()}>
                  {t("entity.undo")}
                </button>
              </p>
            )}
            {splitOff && (
              <p className="note small undo-note">
                <span>{t("entity.splitNote")}</span>
                <button type="button" className="btn btn-small" onClick={() => onSelect(splitOff)}>
                  {t("entity.openSplit")}
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
            sourceKinds={sourceKinds}
            onSelect={onSelect}
            onOpen={opener.open}
            onAct={(c, plan, prev) => void act(c, plan, prev)}
          />
          <Mentions mentions={d.mentions} sourceNames={sourceNames} sourceKinds={sourceKinds} onOpen={opener.open} />
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
          {splitting && (
            <SplitDialog
              target={d}
              onClose={() => setSplitting(false)}
              onSplit={(newId) => {
                setSplitting(false);
                setUndo(null);
                setSplitOff(newId);
                setAnnounce(t("entity.splitNote"));
                reload();
              }}
            />
          )}
        </>
      )}
    </section>
  );
}

function CloseButton({ onClose }: { onClose: () => void }) {
  const t = useT();
  return (
    <button type="button" className="btn btn-ghost btn-icon" onClick={onClose} aria-label={t("entity.closeDetails")}>
      <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true" focusable="false">
        <path d="M2 2l10 10M12 2L2 12" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
      </svg>
    </button>
  );
}

function Header({ detail: d, headingId, onClose }: { detail: EntityDetail; headingId: string; onClose?: () => void }) {
  const t = useT();
  const aliases = d.aliases.filter((a) => a !== d.name);
  const attrs = attributeRows(d.attributes);
  return (
    <>
      <div className="entity-detail-head">
        <div>
          <h2 id={headingId} className="entity-name" dir="auto">
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
              <dt>{t("entity.aliases")}</dt>
              <dd dir="auto">{aliases.join(t("common.listSeparator"))}</dd>
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
              <dd dir="auto">{v}</dd>
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
  const t = useT();
  const links = vaultLinks(vaultRoot, detail.vault_path);
  if (!links) {
    return (
      <button type="button" className="btn btn-small" disabled title={t("entity.noVaultPage")}>
        {t("entity.openInVault")}
      </button>
    );
  }
  const label = t("entity.vaultPageLabel", { name: detail.name });
  return (
    <>
      <button type="button" className="btn btn-small" onClick={() => void open(links.file, label)}>
        {t("entity.openInVault")}
      </button>
      {links.obsidian && (
        <button type="button" className="btn btn-small btn-ghost" onClick={() => void open(links.obsidian!, label)}>
          {t("entity.openInObsidian")}
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
  sourceKinds,
  onSelect,
  onOpen,
  onAct,
}: {
  detail: EntityDetail;
  overrides: Record<number, ClaimStatus>;
  busyClaim: number | null;
  sourceNames: ReadonlyMap<string, string>;
  sourceKinds?: ReadonlyMap<string, string>;
  onSelect: (id: string) => void;
  onOpen: (url: string, label: string) => Promise<void>;
  onAct: (c: ClaimView, plan: FactActionPlan, prev: ClaimStatus) => void;
}) {
  const t = useT();
  const claims = detail.claims.map((c) => (overrides[c.id] ? { ...c, status: overrides[c.id]! } : c));
  // One clock per load, so groups and "3 days ago" agree and don't shift while reading.
  const now = useMemo(() => Date.now(), [detail]);
  // Recency when the daemon sends timestamps; by predicate otherwise.
  const groups = hasClaimTimes(claims)
    ? groupClaimsByRecency(claims, now).map((g) => ({ key: g.bucket, label: t(g.label), claims: g.claims, recency: true }))
    : groupClaims(claims).map((g) => ({ key: g.predicate, label: g.label, claims: g.claims, recency: false }));
  const headingId = useId();
  return (
    <section aria-labelledby={headingId}>
      <h3 id={headingId} className="entity-section">
        <Trans
          k="entity.factsHeading"
          args={{ n: claims.filter((c) => !isDroppedStatus(c.status)).length }}
          tags={{ count: (x) => <span className="muted small">{x}</span> }}
        />
      </h3>
      {groups.length === 0 && <p className="muted small">{t("entity.noFacts", { name: detail.name })}</p>}
      {groups.map((g) => (
        <div key={g.key} className={g.recency ? "fact-group fact-group-recency" : "fact-group"}>
          <h4 className="fact-predicate">{g.label}</h4>
          <ul className="facts">
            {g.claims.map((c) => (
              <Fact
                key={c.id}
                claim={c}
                original={detail.claims.find((x) => x.id === c.id)?.status ?? c.status}
                busy={busyClaim === c.id}
                sourceNames={sourceNames}
                sourceKinds={sourceKinds}
                now={now}
                selfId={detail.id}
                onSelect={onSelect}
                onOpen={onOpen}
                onAct={onAct}
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
  sourceKinds,
  now,
  selfId,
  onSelect,
  onOpen,
  onAct,
}: {
  claim: ClaimView;
  /** Status before any optimistic change, so a failed reject restores it. */
  original: ClaimStatus;
  busy: boolean;
  sourceNames: ReadonlyMap<string, string>;
  sourceKinds?: ReadonlyMap<string, string>;
  now: number;
  selfId: string;
  onSelect: (id: string) => void;
  onOpen: (url: string, label: string) => Promise<void>;
  onAct: (c: ClaimView, plan: FactActionPlan, prev: ClaimStatus) => void;
}) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const evidenceId = useId();
  const text = claimText(c);
  const retracted = isDroppedStatus(c.status);
  const plan = factAction(c);
  const [label, aria] =
    plan.action === "restore"
      ? [t("entity.restore"), t("entity.restoreLabel", { text })]
      : plan.action === "unassert"
        ? [t("entity.unassert"), t("entity.unassertLabel", { text })]
        : [t("entity.reject"), t("entity.rejectLabel", { text })];
  const other = c.object.entity && c.object.entity.id !== selfId ? c.object.entity : null;
  const seen = claimTime(c);
  return (
    <li className={`fact${retracted ? " fact-retracted" : ""}${c.status === "superseded" ? " fact-superseded" : ""}`}>
      <p className="fact-text" dir="auto">
        {retracted ? <s>{text}</s> : text}
        {c.status === "user_asserted" && (
          <span className="fact-you" title={t("entity.youTitle")}>
            {" "}
            {t("entity.you")}
          </span>
        )}
      </p>
      <div className="fact-meta">
        <Badge tone={confidenceTone(c.confidence)}>
          <span className="visually-hidden">{t("entity.confidence")} </span>
          {formatConfidence(c.confidence)}
        </Badge>
        {!isCurrentStatus(c.status) && (
          <Badge tone={retracted ? "bad" : "neutral"}>{claimStatusLabel(c.status)}</Badge>
        )}
        {seen && (
          <span className="muted small fact-seen">
            <span className="visually-hidden">{t("entity.lastSeen")} </span>
            <RelativeTime ts={seen} now={now} />
          </span>
        )}
        {other && (
          <button type="button" className="btn btn-ghost btn-small" onClick={() => onSelect(other.id)}>
            {t("entity.goTo", { name: other.name })}
          </button>
        )}
        {c.evidence.length > 0 ? (
          <button type="button" className="btn btn-ghost btn-small" aria-expanded={open} aria-controls={evidenceId} onClick={() => setOpen((o) => !o)}>
            {t("entity.why")} <span className="muted">{evidenceSummary(c.evidence)}</span>
          </button>
        ) : (
          <span className="muted small">{c.status === "user_asserted" ? t("entity.fromVaultEdit") : evidenceSummary([])}</span>
        )}
        <button
          type="button"
          className={retracted ? "btn btn-small" : "btn btn-small btn-danger-ghost"}
          disabled={busy}
          aria-label={aria}
          onClick={() => onAct(c, plan, original)}
        >
          {busy ? <Spinner label={t("entity.saving")} /> : label}
        </button>
      </div>
      <div id={evidenceId} hidden={!open}>
        {open && (
          <ul className="evidence">
            {c.evidence.map((ev, i) => {
              const e = formatEvidence(ev, sourceNames);
              const at = evidenceTime(ev);
              return (
                <li key={`${ev.item.source}\u0000${ev.item.key}\u0000${ev.chunk_id ?? i}`}>
                  {e.quote ? (
                    <blockquote className="evidence-quote" dir="auto" title={e.truncated ? ev.quote ?? undefined : undefined}>
                      {e.quote}
                    </blockquote>
                  ) : (
                    <p className="muted small">{t("entity.noQuote")}</p>
                  )}
                  <div className="evidence-source">
                    <SourceIcon kind={sourceKindOf(ev.item.source, sourceKinds)} size={16} />
                    <span className="evidence-title" dir="auto">
                      {e.title}
                    </span>
                    <span className="muted small">{e.source}</span>
                    {at && <RelativeTime ts={at} now={now} className="muted small" />}
                    {e.openUrl && (
                      <button type="button" className="btn btn-small" onClick={() => void onOpen(e.openUrl!, e.title)}>
                        {t("entity.open")}
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
  sourceKinds,
  onOpen,
}: {
  mentions: MentionView[];
  sourceNames: ReadonlyMap<string, string>;
  sourceKinds?: ReadonlyMap<string, string>;
  onOpen: (url: string, label: string) => Promise<void>;
}) {
  const t = useT();
  const [all, setAll] = useState(false);
  const headingId = useId();
  const shown = all ? mentions : mentions.slice(0, MENTIONS_SHOWN);
  return (
    <section aria-labelledby={headingId}>
      <h3 id={headingId} className="entity-section">
        <Trans
          k="entity.mentionsHeading"
          args={{ n: mentions.length }}
          tags={{ count: (x) => <span className="muted small">{x}</span> }}
        />
      </h3>
      {mentions.length === 0 && <p className="muted small">{t("entity.noMentions")}</p>}
      <ul className="mentions">
        {shown.map((m, i) => {
          const e = formatEvidence({ item: m.item, chunk_id: m.chunk_id, title: m.title, url: m.url, quote: null }, sourceNames);
          return (
            <li key={`${m.item.source}\u0000${m.item.key}\u0000${m.chunk_id ?? ""}\u0000${i}`}>
              <SourceIcon kind={sourceKindOf(m.item.source, sourceKinds)} size={14} />
              <span className="mention-surface">{t("entity.mentionSurface", { surface: m.surface })}</span>
              <span className="mention-title" dir="auto">
                {e.title}
              </span>
              <span className="muted small">{e.source}</span>
              {e.openUrl && (
                <button type="button" className="btn btn-small btn-ghost" onClick={() => void onOpen(e.openUrl!, e.title)}>
                  {t("entity.open")}
                </button>
              )}
            </li>
          );
        })}
      </ul>
      {mentions.length > MENTIONS_SHOWN && (
        <button type="button" className="btn btn-ghost btn-small" onClick={() => setAll((a) => !a)} aria-expanded={all}>
          {all ? t("entity.showFewer") : t("entity.showAllMentions", { n: mentions.length })}
        </button>
      )}
    </section>
  );
}
