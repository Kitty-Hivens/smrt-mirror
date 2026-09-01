<script lang="ts">
  // What has been checkpointed, as a list.
  //
  // It is a tool rather than a place (ADR 0005): consulted while working on
  // something else, so it opens as a dock over whatever tab is up rather than
  // living inside one of them. What it is not is the commit box -- declaring a
  // checkpoint is the first half of building, so that stays beside the build
  // button, where the same sentence serves both acts.
  import { api } from '../lib/api';
  import { notifyFail, toasts } from '../lib/toasts.svelte';
  import { changeWords, t } from '../lib/i18n.svelte';
  import { dialogs } from '../lib/dialogs.svelte';
  import { route, href, plainClick } from '../lib/route.svelte';
  import { tally } from '../lib/changes';
  import type { Commit, CommitLogEntry } from '../lib/types';

  let {
    packId,
    log,
    onChanged,
    onBuildCommit,
    hasMore = false,
    failed = false,
    loadingMore = false,
    onMore = () => {},
    busy = false,
    working = $bindable(false),
  }: {
    packId: string;
    log: CommitLogEntry[];
    /// The history moved; whoever holds it re-reads.
    onChanged: () => void;
    onBuildCommit: (commitId: string) => void;
    /// Whether the history goes further back than what is on screen.
    hasMore?: boolean;
    /// Whether the read failed, which is not the same as a pack with no history.
    failed?: boolean;
    loadingMore?: boolean;
    onMore?: () => void;
    /// A build is in flight; restoring under one would be a race.
    busy?: boolean;
    working?: boolean;
  } = $props();

  const short = (id: string) => id.slice(0, 8);

  async function restore(entry: CommitLogEntry) {
    working = true;
    try {
      // What a restore would do, before it does it: the commit read against the
      // working state. Pressing "restore" used to be a single click with no
      // statement of consequences anywhere on the way.
      let summary = '';
      try {
        const diff = await api.commitDiff(packId, entry.id, 'live');
        const c = tally(diff.changes);
        summary = diff.changes.length
          ? t('hist.restoreEffect', changeWords(c))
          : t('hist.restoreNoop');
      } catch {
        // an unreadable diff must not block the act; the question still names
        // the commit it is about to put back
        summary = t('hist.restoreUnknown');
      }
      const ok = await dialogs.confirm(
        `${t('hist.restoreAsk', { id: short(entry.id), message: entry.message })}\n\n${summary}`,
        { title: t('hist.restore'), danger: true },
      );
      if (!ok) return;
      await api.restoreCommit(packId, entry.id);
      toasts.push({ kind: 'ok', text: t('hist.restored', { id: short(entry.id) }) });
      onChanged();
    } catch (e) {
      notifyFail(e);
    } finally {
      working = false;
    }
  }

  async function copyId(id: string) {
    try {
      await navigator.clipboard.writeText(id);
      toasts.push({ kind: 'ok', text: t('hist.idCopied') });
    } catch {
      // a browser that refuses the clipboard still shows the id in the title
    }
  }

  // The timestamp as a person reads it. The stored value is RFC 3339 UTC; what
  // is useful on screen is when it was, locally.
  function when(at: string): string {
    const d = new Date(at);
    return Number.isNaN(d.getTime()) ? at : d.toLocaleString();
  }

  function openCommit(e: MouseEvent, c: Commit) {
    if (!plainClick(e)) return;
    e.preventDefault();
    route.openCommit(packId, c.id);
  }
</script>

<div class="hist">
  {#if log.length}
    <ol class="log">
      {#each log as c (c.id)}
        <li>
          <div class="line">
            <a
              class="msg"
              href={href.commit(packId, c.id)}
              onclick={(e) => openCommit(e, c)}
              title={t('hist.openCommit')}
            >
              {c.message.split('\n')[0]}
            </a>
            <button class="id mono" title={c.id} onclick={() => copyId(c.id)}>
              {short(c.id)}
            </button>
          </div>
          <div class="meta muted">
            <span>{c.author}</span>
            <span>{when(c.at)}</span>
            {#if c.contributors.length > 1}
              <span>{t('hist.with', { who: c.contributors.slice(1).join(', ') })}</span>
            {/if}
            {#if c.builds.length}
              <span class="built" title={t('hist.builtFrom')}>{c.builds.join(', ')}</span>
            {:else}
              <span class="unbuilt">{t('hist.neverBuilt')}</span>
            {/if}
            <button class="link" onclick={() => onBuildCommit(c.id)} disabled={busy || working}>
              {t('hist.buildThis')}
            </button>
            <button class="link" onclick={() => restore(c)} disabled={busy || working}>
              {t('hist.restore')}
            </button>
          </div>
        </li>
      {/each}
    </ol>
    {#if hasMore}
      <button class="more" onclick={onMore} disabled={busy || working || loadingMore}>
        {loadingMore ? t('common.loading') : t('hist.more')}
      </button>
    {/if}
  {:else if failed}
    <!-- an unread history and a pack that never declared one look identical
         from an empty list, and only one of them is worth acting on -->
    <p class="muted empty">{t('hist.logUnread')}</p>
  {/if}
</div>

<style>
  .hist {
    font-size: var(--fs-md);
  }
  .empty {
    font-size: var(--fs-sm);
    margin: 14px 0 0;
  }
  .more {
    background: none;
    border: 0;
    padding: 8px 0 0;
    font: inherit;
    font-size: var(--fs-sm);
    color: var(--accent, var(--fg));
    cursor: pointer;
  }
  .more:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .log {
    list-style: none;
    margin: 14px 0 0;
    padding: 0;
    border-top: 1px solid var(--seam);
  }
  .log li {
    padding: 8px 0;
    border-bottom: 1px solid var(--seam);
  }
  .line {
    display: flex;
    gap: 10px;
    align-items: baseline;
    justify-content: space-between;
  }
  .msg {
    overflow-wrap: anywhere;
    color: inherit;
    text-decoration: none;
  }
  .msg:hover {
    text-decoration: underline;
  }
  .id {
    background: none;
    border: 0;
    padding: 0;
    font: inherit;
    color: var(--fg-dim);
    font-size: var(--fs-sm);
    cursor: pointer;
    flex: 0 0 auto;
  }
  .id:hover {
    color: var(--fg);
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    font-size: var(--fs-sm);
    margin-top: 3px;
  }
  .built {
    color: var(--ok, var(--fg-dim));
    font-variant-numeric: tabular-nums;
  }
  .unbuilt {
    opacity: 0.7;
  }
  .link {
    background: none;
    border: 0;
    padding: 0;
    font: inherit;
    color: var(--accent, var(--fg));
    cursor: pointer;
  }
  .link:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
