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
  import TabStrip from './ui/TabStrip.svelte';
  import Glyph from './ui/Glyph.svelte';
  import Avatar from './Avatar.svelte';
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

  const filterTabs = $derived([
    { value: 'open', label: t('thr.filterOpen') },
    { value: 'all', label: t('thr.filterAll') },
  ]);

  function cancel() {
    opening = false;
    proposing = false;
  }

  function when(at: number): string {
    const d = new Date(at * 1000);
    return Number.isNaN(d.getTime()) ? String(at) : d.toLocaleDateString();
  }
</script>

<div class="threads">
  <div class="bar">
    <TabStrip
      variant="pill"
      value={showAll ? 'all' : 'open'}
      tabs={filterTabs}
      ariaLabel={t('browse.discussion')}
      onChange={(v) => (showAll = v === 'all')}
    />
    <div class="acts">
      {#if canWrite && !suspended && !opening}
        <button class="act" onclick={() => (opening = true)}><Glyph name="chat" size={14} />{t('thr.report')}</button>
      {/if}
      {#if forkOf && !proposing}
        <button class="act" onclick={() => (proposing = true)}><Glyph name="fork" size={14} />{t('thr.propose', { pack: forkOf })}</button>
      {/if}
    </div>
  </div>

  {#if suspended}
    <p class="suspended">
      {#if suspended.everywhere}
        {suspended.reason ? t('thr.stopped', { reason: suspended.reason }) : t('thr.stoppedPlain')}
      {:else}
        {suspended.reason ? t('thr.suspendedWhy', { reason: suspended.reason }) : t('thr.suspended')}
      {/if}
    </p>
  {/if}

  {#if opening && canWrite && !suspended}
    <div class="form">
      <input class="prose" bind:value={title} placeholder={t('thr.titlePlaceholder')} disabled={working} />
      <textarea class="prose" rows="4" bind:value={body} placeholder={t('thr.bodyPlaceholder')} disabled={working}
      ></textarea>
      <div class="formacts">
        <button class="primary" onclick={report} disabled={working || !title.trim()}>{t('thr.send')}</button>
        <button onclick={cancel} disabled={working}>{t('thr.cancel')}</button>
      </div>
    </div>
  {/if}

  {#if proposing && forkOf}
    <div class="form">
      <p class="lead">{t('thr.proposeLead', { pack: forkOf })}</p>
      <input class="prose" bind:value={title} placeholder={t('thr.proposeTitle')} disabled={working} />
      <textarea class="prose" rows="4" bind:value={body} placeholder={t('thr.bodyPlaceholder')} disabled={working}
      ></textarea>
      <div class="formacts">
        <button class="primary" onclick={propose} disabled={working || !title.trim()}>{t('thr.send')}</button>
        <button onclick={cancel} disabled={working}>{t('thr.cancel')}</button>
      </div>
    </div>
  {/if}

  {#if loading && !rows.length}
    <Skeleton rows={3} height={64} gap={0} shape="row" lead={28} />
  {:else if failed}
    <p class="lead">{t('thr.unreadable')}</p>
  {:else if !rows.length}
    <!-- an empty discussion is still the place to start one, so it says how -->
    <div class="empty">
      <span class="emark"><Glyph name="chat" size={18} /></span>
      <div>
        <div class="etitle">{showAll ? t('thr.noneAtAll') : t('thr.noneOpen')}</div>
        <p class="esub">{canWrite ? t('thr.emptySub') : t('browse.signInToReport')}</p>
      </div>
    </div>
  {:else}
    <ol class="list">
      {#each rows as r (r.id)}
        <li>
          <a class="row" href={address(r)} onclick={(e) => open(e, r)}>
            <Avatar uid={r.by_uid} login={r.by_login ?? '?'} size={28} />
            <span class="main">
              <span class="ttl">{r.title}</span>
              <span class="meta">
                <span class="kind" data-kind={r.kind}>{t(`thr.kind.${r.kind}` as 'thr.kind.issue')}</span>
                <span>#{r.id}</span>
                <span>{r.by_login ?? t('acc.unknownUser', { uid: r.by_uid })}</span>
                <span>{when(r.created_at)}</span>
              </span>
            </span>
            {#if r.comments}
              <span class="count" title={t('thr.comments', { n: r.comments })}><Glyph name="chat" size={13} />{r.comments}</span>
            {/if}
            <span class="status" data-status={r.status}>{t(`thr.status.${r.status}` as 'thr.status.open')}</span>
          </a>
        </li>
      {/each}
    </ol>
    {#if !canWrite}
      <p class="lead">{t('browse.signInToReport')}</p>
    {/if}
    {#if next}
      <button class="link more" onclick={more} disabled={working}>{t('thr.more')}</button>
    {/if}
  {/if}
</div>

<style>
  .threads {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2) var(--space-3);
  }
  .acts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  .act {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    padding: 6px 14px;
    font-size: var(--fs-md);
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-4);
    background: var(--panel-2);
    border: 1px solid var(--seam);
    border-radius: var(--radius-md);
  }
  .form textarea {
    resize: vertical;
  }
  .formacts {
    display: flex;
    gap: var(--space-2);
  }
  .lead {
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--fg-dim);
  }
  .suspended {
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--fg-dim);
    border-left: 2px solid var(--danger);
    padding-left: 10px;
  }

  .empty {
    display: flex;
    align-items: center;
    gap: var(--space-4);
    padding: var(--space-5);
    border: 1px dashed var(--seam-bright);
    border-radius: var(--radius-md);
  }
  .emark {
    width: 40px;
    height: 40px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: var(--radius-md);
    background: var(--panel-2);
    color: var(--fg-dim);
  }
  .etitle {
    font-size: var(--fs-lg);
    font-weight: 600;
  }
  .esub {
    margin: 2px 0 0;
    font-size: var(--fs-md);
    color: var(--fg-dim);
  }

  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    border: 1px solid var(--seam);
    border-radius: var(--radius-md);
    overflow: hidden;
  }
  .list li + li {
    border-top: 1px solid var(--seam);
  }
  /* a report is a place, so its whole row is the way there */
  .row {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    color: inherit;
    text-decoration: none;
  }
  .row:hover {
    background: var(--panel-2);
  }
  .row:hover .ttl {
    text-decoration: underline;
    text-decoration-color: var(--seam-bright);
    text-underline-offset: 2px;
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .ttl {
    font-size: var(--fs-lg);
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 2px var(--space-3);
    font-size: var(--fs-sm);
    color: var(--fg-faint);
  }
  .kind {
    color: var(--fg-dim);
  }
  .kind[data-kind='proposal'] {
    color: var(--accent-hue);
  }
  .count {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--fs-sm);
    color: var(--fg-dim);
    font-variant-numeric: tabular-nums;
  }
  .status {
    flex: none;
    font-size: var(--fs-xs);
    padding: 2px 9px;
    border-radius: 999px;
    border: 1px solid var(--seam-bright);
    color: var(--fg-dim);
  }
  .status[data-status='open'] {
    color: var(--ok);
    border-color: color-mix(in srgb, var(--ok) 45%, var(--seam));
    background: var(--ok-soft);
  }
  .status[data-status='merged'] {
    color: var(--accent-hue);
    border-color: color-mix(in srgb, var(--accent-hue) 45%, var(--seam));
    background: var(--accent-hue-soft);
  }
  .status[data-status='declined'] {
    color: var(--danger);
    border-color: color-mix(in srgb, var(--danger) 40%, var(--seam));
  }
  .more {
    align-self: flex-start;
    font-size: var(--fs-sm);
  }
  @container view (max-width: 560px) {
    .count {
      display: none;
    }
    .empty {
      padding: var(--space-4);
    }
  }
</style>
