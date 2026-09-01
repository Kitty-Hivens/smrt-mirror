<script lang="ts">
  // One checkpoint, in full: what it recorded, who declared it, what shipped
  // out of it, and the two things anyone wants from an old state -- build it,
  // or put it back.
  //
  // A place rather than a panel (ADR 0005): it has an address, back leaves it,
  // and the link is the answer to "which state is 0.1.31?".
  import { api } from '../lib/api';
  import { notifyFail, toasts } from '../lib/toasts.svelte';
  import { t } from '../lib/i18n.svelte';
  import Skeleton from './ui/Skeleton.svelte';
  import { dialogs } from '../lib/dialogs.svelte';
  import { route } from '../lib/route.svelte';
  import { tally } from '../lib/changes';
  import ChangeList from './ChangeList.svelte';
  import type { CommitDiff, CommitLogEntry, ConfigChange } from '../lib/types';

  let {
    packId,
    commitId,
    building = false,
    onBuildCommit,
    onChanged,
  }: {
    packId: string;
    commitId: string;
    /// Whether a build of this pack is already running. Starting a second one
    /// from here would publish the same pack twice over.
    building?: boolean;
    onBuildCommit: (commitId: string) => void;
    onChanged: () => void;
  } = $props();

  let commit = $state<CommitLogEntry | null>(null);
  let diff = $state<CommitDiff | null>(null);
  let loading = $state(true);
  let failed = $state(false);
  let working = $state(false);
  // Which reading is on screen: what this commit recorded (against its parent),
  // or what separates it from the working state -- the question a restore asks.
  let against = $state<'parent' | 'live'>('parent');
  let labels = $state<Record<string, string>>({});

  // Which read is the current one. Switching the reading twice quickly issues
  // two requests, and the slower one is not always the older: without this the
  // rows of one reading end up under the other's heading.
  let generation = 0;

  $effect(() => {
    void load(packId, commitId, against);
  });

  async function load(pack: string, id: string, mode: 'parent' | 'live') {
    const mine = ++generation;
    loading = true;
    failed = false;
    try {
      const [meta, d] = await Promise.all([
        api.commitById(pack, id),
        api.commitDiff(pack, id, mode === 'live' ? 'live' : undefined),
      ]);
      if (mine !== generation) return;
      commit = meta;
      diff = d;
      void loadLabels(d.changes);
    } catch (e) {
      if (mine !== generation) return;
      // The diff on screen belongs to the reading that failed to replace it, so
      // it is dropped rather than left under the other heading.
      diff = null;
      failed = true;
      notifyFail(e);
    } finally {
      if (mine === generation) loading = false;
    }
  }

  async function loadLabels(rows: ConfigChange[]) {
    const projects = [...new Set(rows.map((r) => r.project).filter((p): p is string => !!p))];
    for (const project of projects) {
      try {
        const versions = await api.modrinthVersions(project);
        const found: Record<string, string> = {};
        for (const v of versions) found[v.id] = v.version_number;
        labels = { ...labels, ...found };
      } catch {
        // ids on screen still say the pin moved and to what
      }
    }
  }

  const subject = $derived(commit?.message.split('\n')[0] ?? '');
  const body = $derived((commit?.message.split('\n').slice(1).join('\n') ?? '').trim());
  const counts = $derived(tally(diff?.changes ?? []));

  async function restore() {
    if (!commit) return;
    working = true;
    try {
      let summary = '';
      try {
        const preview = await api.commitDiff(packId, commit.id, 'live');
        const c = tally(preview.changes);
        summary = preview.changes.length
          ? t('hist.restoreEffect', { add: c.add, remove: c.remove, change: c.change })
          : t('hist.restoreNoop');
      } catch {
        summary = t('hist.restoreUnknown');
      }
      const ok = await dialogs.confirm(
        `${t('hist.restoreAsk', { id: short(commit.id), message: subject })}\n\n${summary}`,
        { title: t('hist.restore'), danger: true },
      );
      if (!ok) return;
      await api.restoreCommit(packId, commit.id);
      toasts.push({ kind: 'ok', text: t('hist.restored', { id: short(commit.id) }) });
      onChanged();
      route.closeCommit();
    } catch (e) {
      notifyFail(e);
    } finally {
      working = false;
    }
  }

  async function copyId() {
    if (!commit) return;
    try {
      await navigator.clipboard.writeText(commit.id);
      toasts.push({ kind: 'ok', text: t('hist.idCopied') });
    } catch {
      // the full id is in the title attribute either way
    }
  }

  const short = (id: string) => id.slice(0, 8);

  function when(at: string): string {
    const d = new Date(at);
    return Number.isNaN(d.getTime()) ? at : d.toLocaleString();
  }
</script>

<div class="page">
  <div class="top">
    <button class="link" onclick={() => route.closeCommit()}>&larr; {t('commit.back')}</button>
  </div>

  {#if loading && !commit}
    <Skeleton rows={1} height={64} />
    <Skeleton rows={4} height={28} gap={4} />
  {:else if failed && !commit}
    <p class="muted">{t('commit.unreadable')}</p>
  {:else if commit}
    <header>
      <h2>{subject}</h2>
      {#if body}
        <p class="body">{body}</p>
      {/if}
      <div class="meta muted">
        <button class="idfull mono" title={commit.id} onclick={copyId}>{commit.id}</button>
        <span>{commit.author}</span>
        <span>{when(commit.at)}</span>
        {#if commit.contributors.length > 1}
          <span>{t('hist.with', { who: commit.contributors.slice(1).join(', ') })}</span>
        {/if}
      </div>
      <div class="ships">
        {#if commit.builds.length}
          <span class="built">{t('commit.builtAs', { versions: commit.builds.join(', ') })}</span>
        {:else}
          <span class="muted">{t('hist.neverBuilt')}</span>
        {/if}
      </div>
    </header>

    <div class="modes">
      <button class:on={against === 'parent'} onclick={() => (against = 'parent')}>
        {t('commit.recorded')}
      </button>
      <button class:on={against === 'live'} onclick={() => (against = 'live')}>
        {t('commit.againstLive')}
      </button>
    </div>

    {#if loading}
      <Skeleton rows={4} height={28} gap={4} />
    {:else if failed}
      <p class="muted">{t('commit.diffUnreadable')}</p>
    {:else if diff && diff.changes.length}
      <p class="muted lead">
        {against === 'live'
          ? t('commit.leadLive', { add: counts.add, remove: counts.remove, change: counts.change })
          : diff.from
            ? t('commit.leadParent', { parent: short(diff.from) })
            : t('commit.leadRoot')}
      </p>
      <ChangeList rows={diff.changes} {labels} />
    {:else if diff}
      <p class="muted">
        {against === 'live' ? t('commit.sameAsLive') : t('commit.recordedNothing')}
      </p>
    {/if}

    <div class="acts">
      <button onclick={() => onBuildCommit(commitId)} disabled={working || building}>
        {building ? t('bld.building') : t('commit.build')}
      </button>
      <button class="danger" onclick={restore} disabled={working || building}>
        {t('commit.restore')}
      </button>
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
  .body {
    margin: 0 0 8px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    align-items: baseline;
    font-size: var(--fs-sm);
  }
  .idfull {
    background: none;
    border: 0;
    padding: 0;
    font: inherit;
    color: var(--fg-dim);
    cursor: pointer;
    overflow-wrap: anywhere;
    text-align: left;
  }
  .idfull:hover {
    color: var(--fg);
  }
  .ships {
    margin-top: 6px;
    font-size: var(--fs-sm);
  }
  .built {
    color: var(--ok, var(--fg));
    font-variant-numeric: tabular-nums;
  }
  .modes {
    display: flex;
    gap: 4px;
    margin: 16px 0 10px;
  }
  .modes button {
    background: none;
    border: 1px solid transparent;
    padding: 2px 9px;
    font: inherit;
    font-size: var(--fs-sm);
    color: var(--fg-dim);
    cursor: pointer;
  }
  .modes button.on {
    border-color: var(--seam);
    color: var(--fg);
  }
  .lead {
    font-size: var(--fs-sm);
    margin: 0 0 8px;
  }
  .acts {
    display: flex;
    gap: 10px;
    margin-top: 18px;
  }
  .danger {
    color: var(--danger, var(--fg));
  }
  .link {
    background: none;
    border: 0;
    padding: 0;
    font: inherit;
    color: var(--accent, var(--fg));
    cursor: pointer;
  }
</style>
