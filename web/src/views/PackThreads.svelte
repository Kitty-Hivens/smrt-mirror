<script lang="ts">
  // What is being said about this pack: reports and proposals in one list.
  //
  // They are one list because they are one thing to a reader -- somebody asking
  // the pack's keepers for something -- and because a proposal that hid in its
  // own tab would be a request nobody stumbles over.
  import { api } from '../lib/api';
  import { notifyFail } from '../lib/toasts.svelte';
  import { t } from '../lib/i18n.svelte';
  import Skeleton from './ui/Skeleton.svelte';
  import { route, href, plainClick } from '../lib/route.svelte';
  import type { Thread } from '../lib/types';

  let {
    packId,
    tick = 0,
    forkOf = null,
    standalone = false,
    canWrite = true,
  }: {
    packId: string;
    tick?: number;
    /// The pack this one was forked from, if any. Its presence is what makes
    /// offering the work back possible at all -- there is somewhere to offer it.
    forkOf?: string | null;
    /// Read from the catalog rather than from inside the pack's editor: a
    /// discussion opened here is a place of its own, not a pane over an editor
    /// the reader may not even be allowed to open.
    standalone?: boolean;
    /// Whether the reader can say anything at all. A guest reads the whole
    /// list and writes nothing.
    canWrite?: boolean;
  } = $props();

  // A page at a time: a pack that has been talked about for a year should not
  // cost a reader the whole year to see what is open this week.
  const PAGE = 50;

  let rows = $state<Thread[]>([]);
  let next = $state<string | null>(null);
  // Why this reader may not write here, when they may not -- asked once when the
  // list opens, so the report button is absent rather than refused.
  let suspended = $state<{ reason?: string; at: number; everywhere: boolean } | null>(null);
  let loading = $state(true);
  let failed = $state(false);
  let showAll = $state(false);
  let working = $state(false);

  // The report form, folded away until wanted: the list is what people come for.
  let opening = $state(false);
  let proposing = $state(false);
  let title = $state('');
  let body = $state('');

  $effect(() => {
    void packId;
    void tick;
    void showAll;
    void load();
  });

  $effect(() => {
    const pack = packId;
    if (!canWrite) {
      suspended = null;
      return;
    }
    void (async () => {
      suspended = await api
        .myPackLevel(pack)
        .then((r) => r.suspended ?? null)
        .catch(() => null);
    })();
  });

  async function load() {
    loading = true;
    try {
      const page = await api.threads(packId, undefined, showAll, PAGE);
      rows = page.rows;
      next = page.next;
      failed = false;
    } catch (e) {
      failed = true;
      notifyFail(e);
    } finally {
      loading = false;
    }
  }

  /// Follow the address the last page named, rather than counting rows we have
  /// already seen -- one opened while somebody reads must not shift the page
  /// under them.
  async function more() {
    if (!next || working) return;
    working = true;
    try {
      const page = await api.threadsPage(next);
      rows = [...rows, ...page.rows];
      next = page.next;
    } catch (e) {
      notifyFail(e);
    } finally {
      working = false;
    }
  }

  async function report() {
    const heading = title.trim();
    if (!heading) return;
    working = true;
    try {
      const opened = await api.openIssue(packId, heading, body.trim());
      title = '';
      body = '';
      opening = false;
      if (standalone) route.readThread(opened.id);
      else route.openThread(packId, opened.id);
    } catch (e) {
      notifyFail(e);
    } finally {
      working = false;
    }
  }

  /// Offer this fork's committed state back to the pack it came from. The
  /// thread lands on that pack, not this one, which is why the page it opens is
  /// over there.
  async function propose() {
    const heading = title.trim();
    if (!heading || !forkOf) return;
    working = true;
    try {
      const opened = await api.openProposal(forkOf, packId, heading, body.trim());
      title = '';
      body = '';
      proposing = false;
      route.openThread(forkOf, opened.id);
    } catch (e) {
      notifyFail(e);
    } finally {
      working = false;
    }
  }

  function address(row: Thread): string {
    return standalone ? href.discussion(row.id) : href.thread(packId, row.id);
  }

  function open(e: MouseEvent, row: Thread) {
    if (!plainClick(e)) return;
    e.preventDefault();
    if (standalone) route.readThread(row.id);
    else route.openThread(packId, row.id);
  }

  function when(at: number): string {
    const d = new Date(at * 1000);
    return Number.isNaN(d.getTime()) ? String(at) : d.toLocaleDateString();
  }
</script>

<div class="threads">
  <div class="bar">
    <button class="link" onclick={() => (showAll = !showAll)}>
      {showAll ? t('thr.showOpen') : t('thr.showAll')}
    </button>
    {#if canWrite && !suspended}
      <button class="link" onclick={() => (opening = !opening)}>{t('thr.report')}</button>
    {/if}
    {#if forkOf}
      <button class="link" onclick={() => (proposing = !proposing)}>
        {t('thr.propose', { pack: forkOf })}
      </button>
    {/if}
  </div>

  {#if suspended}
    <p class="muted small suspended">
      {#if suspended.everywhere}
        {suspended.reason ? t('thr.stopped', { reason: suspended.reason }) : t('thr.stoppedPlain')}
      {:else}
        {suspended.reason ? t('thr.suspendedWhy', { reason: suspended.reason }) : t('thr.suspended')}
      {/if}
    </p>
  {/if}

  {#if opening && canWrite && !suspended}
    <div class="form">
      <input bind:value={title} placeholder={t('thr.titlePlaceholder')} disabled={working} />
      <textarea rows="3" bind:value={body} placeholder={t('thr.bodyPlaceholder')} disabled={working}
      ></textarea>
      <button onclick={report} disabled={working || !title.trim()}>{t('thr.send')}</button>
    </div>
  {/if}

  {#if proposing && forkOf}
    <div class="form">
      <p class="muted small">{t('thr.proposeLead', { pack: forkOf })}</p>
      <input bind:value={title} placeholder={t('thr.proposeTitle')} disabled={working} />
      <textarea rows="3" bind:value={body} placeholder={t('thr.bodyPlaceholder')} disabled={working}
      ></textarea>
      <button onclick={propose} disabled={working || !title.trim()}>{t('thr.send')}</button>
    </div>
  {/if}

  {#if loading && !rows.length}
    <Skeleton rows={3} height={44} gap={0} shape="row" lead={0} />
  {:else if failed}
    <p class="muted">{t('thr.unreadable')}</p>
  {:else if !rows.length}
    <p class="muted empty">{showAll ? t('thr.noneAtAll') : t('thr.noneOpen')}</p>
  {:else}
    <ol class="list">
      {#each rows as r (r.id)}
        <li>
          <div class="line">
            <span class="kind" data-kind={r.kind}>{t(`thr.kind.${r.kind}` as 'thr.kind.issue')}</span>
            <a href={address(r)} onclick={(e) => open(e, r)}>{r.title}</a>
            <span class="status" data-status={r.status}>{t(`thr.status.${r.status}` as 'thr.status.open')}</span>
          </div>
          <div class="meta muted">
            <span>#{r.id}</span>
            <span>{r.by_login ?? t('acc.unknownUser', { uid: r.by_uid })}</span>
            <span>{when(r.created_at)}</span>
            {#if r.comments}
              <span>{t('thr.comments', { n: r.comments })}</span>
            {/if}
          </div>
        </li>
      {/each}
    </ol>
    {#if next}
      <button class="link more" onclick={more} disabled={working}>{t('thr.more')}</button>
    {/if}
  {/if}
</div>

<style>
  .threads {
    max-width: 720px;
    padding: 4px 0;
  }
  .bar {
    display: flex;
    gap: 14px;
    margin-bottom: 10px;
    font-size: var(--fs-sm);
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 14px;
    max-width: 640px;
  }
  .form input,
  .form textarea {
    font: inherit;
    resize: vertical;
  }
  .form button {
    align-self: flex-start;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    border-top: 1px solid var(--seam);
  }
  .list li {
    padding: 8px 0;
    border-bottom: 1px solid var(--seam);
  }
  .line {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .line a {
    color: inherit;
    text-decoration: none;
    overflow-wrap: anywhere;
  }
  .line a:hover {
    text-decoration: underline;
  }
  .kind {
    font-size: var(--fs-sm);
    font-variant: small-caps;
    color: var(--fg-dim);
  }
  .kind[data-kind='proposal'] {
    color: var(--accent, var(--fg));
  }
  .status {
    margin-left: auto;
    font-size: var(--fs-sm);
    color: var(--fg-dim);
  }
  .status[data-status='open'] {
    color: var(--ok, var(--fg));
  }
  .status[data-status='merged'] {
    color: var(--accent, var(--fg));
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    font-size: var(--fs-sm);
    margin-top: 3px;
  }
  .empty,
  .small {
    font-size: var(--fs-sm);
  }
  .suspended {
    border-left: 2px solid var(--danger);
    padding-left: 10px;
    margin: 0 0 10px;
  }
  .more {
    margin-top: 10px;
    font-size: var(--fs-sm);
  }
</style>
