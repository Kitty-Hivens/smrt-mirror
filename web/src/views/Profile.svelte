<script lang="ts">
  import { t } from '../lib/i18n.svelte';
  import Skeleton from './ui/Skeleton.svelte';
  import { inbox } from '../lib/inbox.svelte';
  import { mirror } from '../lib/mirror.svelte';
  import { notifyFail } from '../lib/toasts.svelte';
  import { dialogs } from '../lib/dialogs.svelte';
  import { api } from '../lib/api';
  import { href, plainClick, route } from '../lib/route.svelte';
  import Avatar from './Avatar.svelte';
  import type { Notification } from '../lib/types';

  // Who you are, what you are, and what the mirror has to tell you. The last one
  // is here rather than in a surface of its own: being answered is personal, and
  // this is the account's own page.
  type Me = { uid: number; login: string; role: string };
  let { me }: { me: Me } = $props();

  let working = $state(false);
  // The feed address, shown only when asked for: it is a secret in a URL, and
  // minting one for somebody who never wanted it is one more thing to leak.
  let feed = $state<string | null>(null);
  let copied = $state(false);

  $effect(() => {
    // a discussion moving is a pack change, so this re-reads on the same event
    void mirror.packs;
    void inbox.refresh();
  });

  async function open(e: MouseEvent, row: Notification) {
    if (!plainClick(e)) return;
    e.preventDefault();
    // Read it by opening it: a line you have looked at is not news any more.
    if (!row.read) void inbox.markRead(row.id).catch(() => {});
    route.readThread(row.thread_id);
  }

  async function readAll() {
    working = true;
    try {
      await inbox.markRead();
    } catch (e) {
      notifyFail(e);
    } finally {
      working = false;
    }
  }

  /// What happened, in one line. The kind decides the verb; the thread carries
  /// its own words, so a title edited since says the new one.
  function line(row: Notification): string {
    const who = row.actor_login ?? (row.actor_uid === 0 ? t('common.operator') : t('acc.unknownUser', { uid: row.actor_uid }));
    if (row.kind === 'comment') return t('inbox.said', { who });
    if (row.kind === 'opened') return t('inbox.opened', { who, pack: row.pack_id });
    return t('inbox.settled', { who, status: t(`thr.status.${row.status}` as 'thr.status.open') });
  }

  async function showFeed() {
    working = true;
    try {
      feed = (await api.feedKey()).url;
    } catch (e) {
      notifyFail(e);
    } finally {
      working = false;
    }
  }

  async function rotateFeed() {
    if (!(await dialogs.confirm(t('inbox.feedRotateAsk'), { title: t('inbox.feedRotate'), danger: true })))
      return;
    working = true;
    try {
      feed = (await api.rotateFeedKey()).url;
      copied = false;
    } catch (e) {
      notifyFail(e);
    } finally {
      working = false;
    }
  }

  async function copyFeed() {
    if (!feed) return;
    try {
      await navigator.clipboard.writeText(feed);
      copied = true;
    } catch {
      // a browser that refuses the clipboard still shows the address to select
    }
  }

  function when(at: number): string {
    const d = new Date(at * 1000);
    return Number.isNaN(d.getTime()) ? String(at) : d.toLocaleString();
  }
</script>

<div class="view">
  <div class="panel card">
    <Avatar uid={me.uid} login={me.login} size={72} />
    <div class="info">
      <div class="login">{me.login}</div>
      <div class="meta muted mono">uid {me.uid}</div>
    </div>
    <span class="chip role-{me.role}">{me.role}</span>
  </div>

  <section class="panel inbox">
    <header>
      <h3>{t('inbox.title')}</h3>
      {#if inbox.unread}
        <span class="count">{inbox.unread}</span>
        <button class="link" onclick={readAll} disabled={working}>{t('inbox.readAll')}</button>
      {/if}
    </header>
    {#if !inbox.rows.length && !inbox.loaded}
      <Skeleton rows={3} height={40} gap={0} shape="row" lead={0} />
    {:else if !inbox.rows.length}
      <p class="muted empty">{t('inbox.none')}</p>
    {:else}
      <ul>
        {#each inbox.rows as row (row.id)}
          <li class:unread={!row.read}>
            <a href={href.discussion(row.thread_id)} onclick={(e) => open(e, row)}>{row.title}</a>
            <span class="muted what">{line(row)}</span>
            <span class="muted at">{when(row.created_at)}</span>
          </li>
        {/each}
      </ul>
      {#if inbox.hasMore}
        <button class="link more" onclick={() => inbox.more().catch(notifyFail)} disabled={working}>
          {t('thr.more')}
        </button>
      {/if}
    {/if}

    <div class="feed">
      {#if feed}
        <p class="muted small">{t('inbox.feedLead')}</p>
        <div class="feedrow">
          <input class="mono" readonly value={feed} onfocus={(e) => e.currentTarget.select()} />
          <button class="link" onclick={copyFeed}>{copied ? t('inbox.copied') : t('inbox.copy')}</button>
          <button class="link danger" onclick={rotateFeed} disabled={working}>
            {t('inbox.feedRotate')}
          </button>
        </div>
      {:else}
        <button class="link" onclick={showFeed} disabled={working}>{t('inbox.feedShow')}</button>
      {/if}
    </div>
  </section>
</div>

<style>
  .view {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .card {
    display: flex;
    align-items: center;
    gap: var(--space-4);
    padding: var(--space-5);
  }
  .info {
    flex: 1;
    min-width: 0;
  }
  .login {
    font-size: var(--fs-xl);
    font-weight: 700;
  }
  .meta {
    font-size: var(--fs-sm);
    margin-top: 3px;
  }
  .chip {
    font-size: var(--fs-xs);
    padding: 2px 10px;
    border: 1px solid var(--seam);
    border-radius: 999px;
    color: var(--fg-dim);
    flex-shrink: 0;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-family: var(--mono);
  }
  .inbox {
    padding: var(--space-4) var(--space-5);
  }
  .inbox header {
    display: flex;
    align-items: baseline;
    gap: var(--space-3);
    margin-bottom: var(--space-2);
  }
  .inbox h3 {
    margin: 0;
    font-size: var(--fs-md);
  }
  .inbox .count {
    font-family: var(--mono);
    font-size: var(--fs-xs);
    color: var(--on-solid);
    background: var(--solid);
    border-radius: 999px;
    padding: 1px 8px;
  }
  .inbox header .link {
    margin-left: auto;
    font-size: var(--fs-sm);
  }
  .inbox ul {
    list-style: none;
    margin: 0;
    padding: 0;
    border-top: 1px solid var(--seam);
  }
  .inbox li {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--space-3);
    padding: 8px 0 8px 10px;
    border-bottom: 1px solid var(--seam);
    border-left: 2px solid transparent;
    font-size: var(--fs-sm);
  }
  /* what has not been read is marked, not merely ordered: a list where every
     line looks the same is one nobody scans twice */
  .inbox li.unread {
    border-left-color: var(--accent);
  }
  .inbox li a {
    color: inherit;
    font-weight: 500;
    overflow-wrap: anywhere;
  }
  .inbox .at {
    margin-left: auto;
    font-family: var(--mono);
    font-size: var(--fs-xs);
  }
  .inbox .more {
    margin-top: var(--space-2);
    font-size: var(--fs-sm);
  }
  .feed {
    margin-top: var(--space-3);
    padding-top: var(--space-3);
    border-top: 1px solid var(--seam);
  }
  .feed .small {
    margin: 0 0 6px;
  }
  .feedrow {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    flex-wrap: wrap;
  }
  .feedrow input {
    flex: 1;
    min-width: 240px;
    font-size: var(--fs-sm);
  }
  .inbox .empty {
    font-size: var(--fs-sm);
    margin: 0;
  }
  .chip.role-admin {
    color: var(--info);
    border-color: color-mix(in srgb, var(--info) 45%, var(--seam));
    background: var(--info-soft);
  }
  .chip.role-debug {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 45%, var(--seam));
    background: var(--warn-soft);
  }
</style>
