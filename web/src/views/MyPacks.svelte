<script lang="ts">
  import { api, ApiError } from '../lib/api';
  import { notifyFail, toasts } from '../lib/toasts.svelte';
  import { dialogs } from '../lib/dialogs.svelte';
  import { idError, say } from '../lib/validate';
  import { t } from '../lib/i18n.svelte';
  import { mirror } from '../lib/mirror.svelte';
  import { terms } from '../lib/terms.svelte';
  import { href, plainClick, route } from '../lib/route.svelte';
  import type { PackSummary, UploadRow } from '../lib/types';
  import PackEditor from './PackEditor.svelte';

  // A member's own packs: create a community pack and author it. The pack id is
  // u/<uid>/<name> -- the owner is encoded in the id, so the backend gates edits
  // by namespace. Every account has this surface, admins included: a personal
  // community pack under their own uid, separate from the official packs they
  // author via the operator Packs view (#16).
  type Me = { uid: number; login: string; role: string };
  let { me }: { me: Me } = $props();

  let summaries = $state<PackSummary[]>([]);
  let authoring = $state<string[]>([]);
  let uploads = $state<UploadRow[]>([]);
  let loading = $state(true);

  async function load() {
    loading = true;
    try {
      const [s, a, u] = await Promise.all([api.mePacks(), api.meAuthoring(), api.myUploads()]);
      summaries = s;
      authoring = a;
      uploads = u;
    } catch (e) {
      notifyFail(e);
    } finally {
      loading = false;
    }
  }
  load();

  const summaryFor = (id: string) => summaries.find((p) => p.pack_id === id);
  const allIds = $derived(
    [...new Set([...summaries.map((p) => p.pack_id), ...authoring])].sort(),
  );
  // the stored id is u/<uid>/<name>; show just the pack name
  const nameOf = (id: string) => id.split('/').pop() ?? id;

  const visKey = {
    draft: 'packs.vis.draft',
    unlisted: 'packs.vis.unlisted',
    published: 'packs.vis.published',
  } as const;

  async function create() {
    if (!(await terms.ensure())) return;
    const name = (
      await dialogs.prompt(t('mypacks.newPrompt'), { title: t('mypacks.new') })
    )?.trim();
    if (!name) return;
    // The same rule the mirror applies to the id segment. Without it a name
    // carrying a slash or a leading dot opens an editor that refuses its first
    // save -- after the work is in it.
    const bad = say(idError(name));
    if (bad) {
      toasts.push({ kind: 'error', text: bad });
      return;
    }
    route.openPack(`u/${me.uid}/${name}`);
  }

  // publish/unpublish a built pack: a member's pack starts as a draft, off the
  // public Community listing, until they publish it here.
  async function togglePublish(p: PackSummary) {
    const next = p.visibility === 'published' ? 'draft' : 'published';
    try {
      await api.setVisibility(p.pack_id, next);
      await load();
    } catch (e) {
      notifyFail(e);
    }
  }


  // a build of one of these packs, or its deletion, lands here
  $effect(() => {
    if (mirror.packs > 0) load();
  });

  // the list behind the editor is stale once editing ends, whichever way it was
  // left -- the Close button, back, or the trackpad gesture
  let editorWasOpen = false;
  $effect(() => {
    const open = route.pack !== null;
    if (editorWasOpen && !open) load();
    editorWasOpen = open;
  });
</script>

{#if route.pack !== null}
  {#key route.pack}
    <PackEditor packId={route.pack} {me} onClose={() => route.closePack()} />
  {/key}
{:else}
  <div class="view">
    <div class="bar">
      <button class="primary" onclick={create}>{t('mypacks.new')}</button>
    </div>
    <div class="panel list">
      {#each allIds as id (id)}
        {@const p = summaryFor(id)}
        <a
          class="row"
          href={href.pack(id, 'mypacks')}
          onclick={(e) => {
            if (!plainClick(e)) return;
            e.preventDefault();
            route.openPack(id);
          }}
        >
          <span class="av mono">{nameOf(id).slice(0, 2).toUpperCase()}</span>
          <div class="col">
            <div class="nm">{p?.display_name ?? nameOf(id)}</div>
            <div class="mm faint mono">
              {p?.minecraft_version ?? '-'} &middot; {p?.latest_pack_version ?? t('packs.unbuilt')}
            </div>
          </div>
          <div class="grow"></div>
          {#if p}
            <span class="tag vis-{p.visibility}">{t(visKey[p.visibility])}</span>
            <button
              class="pub"
              onclick={(e) => {
                e.stopPropagation();
                togglePublish(p);
              }}>{p.visibility === 'published' ? t('packs.unpublish') : t('packs.publish')}</button>
          {:else}
            <span class="tag">{t('packs.unbuilt')}</span>
          {/if}
        </a>
      {/each}
      {#if allIds.length === 0 && !loading}
        <div class="empty muted">{t('mypacks.empty')}</div>
      {/if}
    </div>

    {#if uploads.length}
      <div class="panel uploads">
        <div class="uptitle mono">{t('mypacks.uploads')}</div>
        {#each uploads as u (u.id)}
          <div class="uprow">
            <span class="upname mono">{u.filename}</span>
            <span class="grow"></span>
            {#if u.note}<span class="upnote faint">{u.note}</span>{/if}
            <span class="upst st-{u.status}">{u.status}</span>
          </div>
        {/each}
      </div>
    {/if}
  </div>
{/if}

<style>
  .view {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .err {
    color: var(--danger);
    font-size: var(--fs-sm);
  }
  .bar {
    margin-bottom: 4px;
  }
  .list {
    overflow: hidden;
  }
  .row {
    display: flex;
    color: inherit;
    text-decoration: none;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3);
    border-bottom: 1px solid var(--seam);
    cursor: pointer;
  }
  .row:last-child {
    border-bottom: none;
  }
  .row:hover {
    background: var(--panel-2);
  }
  .row:focus-visible {
    outline: 2px solid var(--fg);
    outline-offset: -2px;
  }
  .av {
    width: 32px;
    height: 32px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    background: var(--panel-3);
    color: var(--fg-dim);
    font-size: var(--fs-sm);
    font-weight: 700;
  }
  .col {
    min-width: 0;
  }
  .nm {
    font-size: var(--fs-lg);
    font-weight: 600;
  }
  .mm {
    font-size: var(--fs-xs);
    margin-top: 2px;
  }
  .grow {
    flex: 1;
  }
  .tag {
    font-size: var(--fs-xs);
    padding: 2px 8px;
    border: 1px solid var(--seam);
    border-radius: 999px;
    color: var(--fg-dim);
    flex-shrink: 0;
    white-space: nowrap;
  }
  .vis-published {
    color: var(--ok);
  }
  .vis-draft {
    color: var(--fg-faint);
  }
  .vis-unlisted {
    color: var(--accent);
  }
  .pub {
    flex-shrink: 0;
    padding: 4px 10px;
    font-size: var(--fs-xs);
  }
  .empty {
    padding: var(--space-4);
    font-size: var(--fs-sm);
  }
  .uploads {
    overflow: hidden;
  }
  .uptitle {
    font-size: var(--fs-xs);
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--fg-faint);
    padding: var(--space-3);
    border-bottom: 1px solid var(--seam);
  }
  .uprow {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--seam);
    font-size: var(--fs-sm);
  }
  .uprow:last-child {
    border-bottom: none;
  }
  .upname {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .upnote {
    font-size: var(--fs-xs);
  }
  .upst {
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    flex-shrink: 0;
  }
  .st-approved {
    color: var(--ok);
  }
  .st-rejected {
    color: var(--danger);
  }
  .st-pending {
    color: var(--fg-faint);
  }
</style>
