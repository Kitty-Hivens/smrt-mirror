<script lang="ts">
  import { api } from '../lib/api';
  import { notifyFail } from '../lib/toasts.svelte';
  import { t } from '../lib/i18n.svelte';
  import { mirror } from '../lib/mirror.svelte';
  import type { AuditRow } from '../lib/types';

  let rows = $state<AuditRow[]>([]);
  let loading = $state(true);
  // the address of the next page, or null once the trail runs out
  let more = $state<string | null>(null);

  async function load() {
    loading = true;
    try {
      const page = await api.auditLog();
      rows = page.rows;
      more = page.next;
    } catch (e) {
      notifyFail(e);
    } finally {
      loading = false;
    }
  }
  load();

  // Walk further back. The trail only grows at its head, so a page fetched now
  // holds still even as new entries land above it -- older entries keep their
  // place, which is why this appends rather than reloads.
  async function loadMore() {
    if (!more || loading) return;
    loading = true;
    try {
      const page = await api.auditPage(more);
      rows = [...rows, ...page.rows];
      more = page.next;
    } catch (e) {
      notifyFail(e);
    } finally {
      loading = false;
    }
  }

  // anything anyone does gets written here, so any kind of change dates this view
  $effect(() => {
    if (mirror.registry + mirror.packs + mirror.moderation > 0) load();
  });

  // unix seconds -> "YYYY-MM-DD HH:MM" (UTC), enough to place an action in time
  function when(unix: number): string {
    return new Date(unix * 1000).toISOString().slice(0, 16).replace('T', ' ');
  }
</script>

<div class="view">

  <div class="panel alist">
    {#each rows as r (r.id)}
      <div class="arow">
        <span class="chip mono">{r.action}</span>
        <div class="ainfo">
          <div class="who">
            {r.actor_login} <span class="muted mono">uid {r.actor_uid}</span>
          </div>
          {#if r.target}
            <div class="tgt muted mono" title={r.detail ? `${r.target} · ${r.detail}` : r.target}>
              {r.target}{#if r.detail}&nbsp;&middot; {r.detail}{/if}
            </div>
          {/if}
        </div>
        <span class="when muted mono">{when(r.created_at)}</span>
      </div>
    {/each}
    {#if rows.length === 0 && !loading}
      <div class="empty muted">{t('audit.empty')}</div>
    {/if}
  </div>
  {#if more}
    <button class="sm more" onclick={loadMore} disabled={loading}>{t('audit.more')}</button>
  {/if}
</div>

<style>
  .view {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .alist {
    overflow: hidden;
  }
  .more {
    align-self: center;
  }
  /* One column width for the action, whatever its length, so every row's
     text starts at the same place. */
  .arow {
    display: grid;
    grid-template-columns: 15ch minmax(0, 1fr) auto;
    align-items: start;
    gap: var(--space-1) var(--space-3);
    padding: var(--space-3);
    border-bottom: 1px solid var(--seam);
  }
  .arow:last-child {
    border-bottom: none;
  }
  .chip {
    font-size: var(--fs-xs);
    padding: 1px 8px;
    border: 1px solid var(--seam);
    border-radius: 999px;
    color: var(--fg-dim);
    letter-spacing: 0.04em;
    justify-self: start;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    margin-top: 2px;
  }
  .ainfo {
    min-width: 0;
  }
  .who {
    font-size: var(--fs-md);
    font-weight: 600;
  }
  /* one line: a commit hash broken over five lines says less than its
     first twelve characters, and the whole of it is in the tooltip */
  .tgt {
    font-size: var(--fs-xs);
    margin-top: 2px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .when {
    font-size: var(--fs-xs);
    white-space: nowrap;
    margin-top: 3px;
  }
  /* A narrow column puts the action and the time on the first line and gives
     who did what the whole width under them. */
  @container view (max-width: 600px) {
    .arow {
      grid-template-columns: minmax(0, 1fr) auto;
    }
    .ainfo {
      grid-column: 1 / -1;
      grid-row: 2;
    }
  }
  .empty {
    padding: var(--space-4);
    font-size: var(--fs-sm);
  }
</style>
