<script lang="ts">
  // One discussion: what was asked, what was said, and what was decided.
  //
  // A proposal shows the same page plus what taking it would do to this pack as
  // it stands now -- the review question is "what happens to my pack if I take
  // this", and the answer moves as the pack moves.
  import { api } from '../lib/api';
  import { notifyFail, toasts } from '../lib/toasts.svelte';
  import { changeWords, t } from '../lib/i18n.svelte';
  import Skeleton from './ui/Skeleton.svelte';
  import { dialogs } from '../lib/dialogs.svelte';
  import { route } from '../lib/route.svelte';
  import { nameOf as who } from '../lib/people';
  import { tally } from '../lib/changes';
  import ChangeList from './ChangeList.svelte';
  import type { CommitDiff, PackLevel, ThreadView } from '../lib/types';

  let {
    threadId,
    onChanged,
  }: {
    threadId: number;
    /// Told when the discussion moved, for whatever is listing it behind this.
    onChanged?: () => void;
  } = $props();

  // A discussion arrives a page at a time; the thread rides with every page, so
  // what is appended here is the comments and nothing else.
  const PAGE = 100;

  let view = $state<ThreadView | null>(null);
  let older = $state<string | null>(null);
  let diff = $state<CommitDiff | null>(null);
  let loading = $state(true);
  let failed = $state(false);
  let working = $state(false);
  let reply = $state('');
  // Who is reading, and what they may do here -- both from the mirror rather
  // than guessed from the surface this page happens to be mounted on. The same
  // page serves a guest in the catalog and a keeper in the editor, and only the
  // gate knows which is which.
  let me = $state<{ uid: number; login: string } | null>(null);
  let level = $state<PackLevel | null>(null);
  // Why this reader may not write here, when they may not. Asked for rather than
  // discovered by being refused: a reply box that cannot work is a worse way to
  // learn it than a line saying so.
  let suspended = $state<{ reason?: string; at: number; everywhere: boolean } | null>(null);

  const thread = $derived(view?.thread ?? null);
  const isProposal = $derived(thread?.kind === 'proposal');
  const isOpen = $derived(thread?.status === 'open');
  const counts = $derived(tally(diff?.changes ?? []));
  /// Whether this viewer keeps the pack: closes, merges, moderates, blocks.
  const canEdit = $derived(level === 'edit' || level === 'own');
  /// Their own report or proposal, which they may withdraw without keeping the
  /// pack.
  const mine = $derived(me != null && thread != null && thread.by_uid === me.uid);
  /// A discussion standing on its own has no editor behind it, so it says which
  /// pack it is about; inside the editor that would only repeat the header.
  const alone = $derived(route.pack === null);
  /// Where each person first appears, so "block" is offered once per person
  /// rather than once per line they wrote. The thread's own author is offered it
  /// in the header, so their comments never repeat it.
  const firstSaid = $derived.by(() => {
    const first = new Map<number, number>();
    for (const c of view?.comments ?? []) {
      if (!first.has(c.by_uid)) first.set(c.by_uid, c.id);
    }
    return first;
  });

  function blockable(uid: number, commentId: number): boolean {
    if (!canEdit || uid === me?.uid || uid === thread?.by_uid) return false;
    return firstSaid.get(uid) === commentId;
  }

  $effect(() => {
    void load(threadId);
  });

  api
    .me()
    .then((m) => (me = m))
    .catch(() => (me = null));

  async function load(id: number) {
    loading = true;
    try {
      const page = await api.thread(id, PAGE);
      view = page.value;
      older = page.next;
      failed = false;
      // The diff is the reviewer's half of a proposal; an issue has none, and a
      // settled proposal's offer is history rather than a question.
      diff = view.thread.kind === 'proposal' ? await api.threadDiff(id).catch(() => null) : null;
      const standing = await api
        .myPackLevel(view.thread.pack_id)
        .catch(() => ({ level: undefined, suspended: undefined }));
      level = standing.level ?? null;
      suspended = standing.suspended ?? null;
    } catch (e) {
      failed = true;
      notifyFail(e);
    } finally {
      loading = false;
    }
  }

  async function act(run: () => Promise<unknown>) {
    working = true;
    try {
      await run();
      await load(threadId);
      onChanged?.();
    } catch (e) {
      notifyFail(e);
    } finally {
      working = false;
    }
  }

  /// The next page of what was said. Followed by address rather than by
  /// counting: a comment arriving while somebody reads must not shift the page
  /// under them.
  async function readMore() {
    if (!older || working) return;
    working = true;
    try {
      const page = await api.threadPage(older);
      if (view) view = { ...view, comments: [...view.comments, ...page.value.comments] };
      older = page.next;
    } catch (e) {
      notifyFail(e);
    } finally {
      working = false;
    }
  }

  /// Saying something appends it rather than re-reading the discussion: the
  /// mirror answers with the row as a reader will see it, and a reload would
  /// throw away every page after the first -- including, on a long thread, the
  /// page the new comment is on.
  async function say() {
    const body = reply.trim();
    if (!body) return;
    working = true;
    try {
      const added = await api.comment(threadId, body);
      reply = '';
      if (view) view = { ...view, comments: [...view.comments, added] };
      onChanged?.();
    } catch (e) {
      notifyFail(e);
    } finally {
      working = false;
    }
  }

  async function merge() {
    const c = counts;
    const ok = await dialogs.confirm(
      t('thr.mergeAsk', changeWords(c)),
      { title: t('thr.merge') },
    );
    if (!ok) return;
    await act(async () => {
      await api.mergeProposal(threadId);
      toasts.push({ kind: 'ok', text: t('thr.merged') });
    });
  }

  async function hide(commentId: number, hidden: boolean) {
    if (hidden && !(await dialogs.confirm(t('thr.hideAsk'), { title: t('thr.hide'), danger: true }))) {
      return;
    }
    await act(() => api.hideComment(commentId, hidden));
  }

  /// Stop somebody writing on this pack. Hiding answers what was already said;
  /// this answers the next one, which is the only thing that ends a flood.
  async function block(uid: number, who: string) {
    const pack = thread?.pack_id;
    if (!pack) return;
    const reason = await dialogs.prompt(t('thr.blockAsk', { who }), {
      title: t('thr.block'),
      placeholder: t('thr.blockReason'),
    });
    if (reason == null) return;
    await act(async () => {
      await api.blockFromPack(pack, uid, reason.trim() || undefined);
      toasts.push({ kind: 'ok', text: t('thr.blocked', { who }) });
    });
  }

  function when(at: number): string {
    const d = new Date(at * 1000);
    return Number.isNaN(d.getTime()) ? String(at) : d.toLocaleString();
  }
</script>

<div class="page">
  <div class="top">
    <button class="link" onclick={() => route.closeThread()}>
      &larr; {alone ? t('thr.backAlone') : t('thr.back')}
    </button>
  </div>

  {#if loading && !view}
    <Skeleton rows={1} height={56} />
    <Skeleton rows={3} height={64} />
  {:else if failed && !view}
    <p class="muted">{t('thr.unreadable')}</p>
  {:else if thread}
    <header>
      <h2>{thread.title}</h2>
      <div class="meta muted">
        <span class="kind" data-kind={thread.kind}>{t(`thr.kind.${thread.kind}` as 'thr.kind.issue')}</span>
        <span class="status" data-status={thread.status}>{t(`thr.status.${thread.status}` as 'thr.status.open')}</span>
        <span>#{thread.id}</span>
        <span class="name">{who(thread.by_uid, thread.by_login)}</span>
        <span>{when(thread.created_at)}</span>
        {#if alone}
          <span class="mono">{t('thr.onPack', { pack: thread.pack_id })}</span>
        {/if}
        {#if canEdit && thread.by_uid !== me?.uid}
          <button class="link small" onclick={() => block(thread.by_uid, who(thread.by_uid, thread.by_login))} disabled={working}>
            {t('thr.block')}
          </button>
        {/if}
      </div>
      {#if thread.body}
        <p class="body">{thread.body}</p>
      {/if}
      <hr />
      {#if thread.merged_commit}
        <p class="muted small">
          {t('thr.mergedAs', { commit: thread.merged_commit.slice(0, 8) })}
        </p>
      {/if}
    </header>

    {#if isProposal}
      <section class="offer">
        <h3>{t('thr.offers')}</h3>
        {#if diff && diff.changes.length}
          <p class="muted small">
            {t('thr.offersLead', changeWords(counts))}
          </p>
          <ChangeList rows={diff.changes} />
        {:else if diff}
          <p class="muted small">{t('thr.offersNothing')}</p>
        {:else}
          <p class="muted small">{t('thr.offersUnreadable')}</p>
        {/if}
      </section>
    {/if}

    <section class="talk">
      {#each view?.comments ?? [] as c (c.id)}
        <article class:hidden={c.hidden}>
          <div class="who muted">
            <span class="name">{who(c.by_uid, c.by_login)}</span>
            <span>{when(c.created_at)}</span>
            {#if canEdit}
              <span class="mod">
                <button class="link small" onclick={() => hide(c.id, !c.hidden)} disabled={working}>
                  {c.hidden ? t('thr.show') : t('thr.hide')}
                </button>
                {#if blockable(c.by_uid, c.id)}
                  <button
                    class="link small"
                    onclick={() => block(c.by_uid, who(c.by_uid, c.by_login))}
                    disabled={working}
                  >
                    {t('thr.block')}
                  </button>
                {/if}
              </span>
            {/if}
          </div>
          {#if c.hidden}
            <p class="muted taken">{t('thr.taken')}</p>
          {:else}
            <p class="said">{c.body}</p>
          {/if}
        </article>
      {/each}

      {#if older}
        <button class="link more" onclick={readMore} disabled={working}>{t('thr.more')}</button>
      {/if}

      {#if suspended}
        <!-- Said in the discussion rather than in a toast: it is a standing
             fact about this pack, not a failed request. -->
        <p class="muted small say suspended">
          {#if suspended.everywhere}
            {suspended.reason
              ? t('thr.stopped', { reason: suspended.reason })
              : t('thr.stoppedPlain')}
          {:else}
            {suspended.reason
              ? t('thr.suspendedWhy', { reason: suspended.reason })
              : t('thr.suspended')}
          {/if}
        </p>
      {:else if me}
        <div class="say">
          <textarea
            rows="3"
            bind:value={reply}
            placeholder={t('thr.replyPlaceholder')}
            disabled={working}
            onkeydown={(e) => {
              if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) say();
            }}
          ></textarea>
          <button onclick={say} disabled={working || !reply.trim()}>{t('thr.reply')}</button>
        </div>
      {:else}
        <!-- A guest reads the whole discussion and joins none of it: saying
             something here is signed, because a decision has to be answerable
             to somebody. -->
        <p class="muted small say">{t('thr.signInToReply')}</p>
      {/if}
    </section>

    <div class="acts">
      {#if isProposal && isOpen && canEdit}
        <button class="primary" onclick={merge} disabled={working}>{t('thr.merge')}</button>
      {/if}
      {#if isOpen && (canEdit || mine)}
        <button onclick={() => act(() => api.closeThread(threadId))} disabled={working}>
          {isProposal ? (mine && !canEdit ? t('thr.withdraw') : t('thr.decline')) : t('thr.close')}
        </button>
      {:else if !isOpen && thread.kind === 'issue' && (canEdit || mine)}
        <button onclick={() => act(() => api.reopenThread(threadId))} disabled={working}>
          {t('thr.reopen')}
        </button>
      {/if}
    </div>
  {/if}
</div>

<style>
  .page {
    padding: 4px 0 20px;
    max-width: 720px;
  }
  .top {
    margin-bottom: 12px;
  }
  h2 {
    margin: 0 0 6px;
    font-size: var(--fs-lg, 1.1rem);
    overflow-wrap: anywhere;
  }
  h3 {
    margin: 0 0 6px;
    font-size: var(--fs-sm);
    color: var(--fg-dim);
    font-weight: 500;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    align-items: baseline;
    font-size: var(--fs-sm);
  }
  .kind {
    font-variant: small-caps;
  }
  .kind[data-kind='proposal'] {
    color: var(--accent, var(--fg));
  }
  .status[data-status='open'] {
    color: var(--ok, var(--fg));
  }
  .status[data-status='merged'] {
    color: var(--accent, var(--fg));
  }
  hr {
    border: 0;
    border-top: 1px solid var(--seam);
    margin: 14px 0 0;
  }
  .body {
    margin: 8px 0 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .offer {
    margin: 18px 0;
    padding: 10px 12px;
    border: 1px solid var(--seam);
  }
  .talk {
    margin-top: 18px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .talk article {
    padding: 8px 10px;
    border: 1px solid var(--seam);
    border-left-width: 2px;
  }
  .talk article.hidden {
    border-left-color: var(--danger);
    opacity: 0.75;
  }
  .who {
    display: flex;
    gap: 12px;
    align-items: baseline;
    font-size: var(--fs-sm);
  }
  .who .name {
    color: var(--fg);
    font-weight: 500;
  }
  /* The moderator's controls sit at the end of the line, not against the name:
     glued to it they read as part of the byline. */
  .who .mod {
    margin-left: auto;
    display: flex;
    gap: 12px;
  }
  .said {
    margin: 4px 0 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .taken {
    margin: 4px 0 0;
    font-size: var(--fs-sm);
    font-style: italic;
  }
  .suspended {
    border-left: 2px solid var(--danger);
    padding-left: 10px;
  }
  .more {
    align-self: flex-start;
    font-size: var(--fs-sm);
  }
  .say {
    display: flex;
    flex-direction: column;
    gap: 6px;
    align-items: flex-start;
    padding: 12px 0;
  }
  .say textarea {
    font: inherit;
    resize: vertical;
    width: 100%;
    max-width: 640px;
  }
  .acts {
    display: flex;
    gap: 10px;
    margin-top: 8px;
  }
  .link.small {
    font-size: var(--fs-sm);
  }
</style>
