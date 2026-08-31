<script lang="ts">
  import { api, ApiError } from '../lib/api';
  import { countUp } from '../lib/motion.svelte';
  import { notifyFail } from '../lib/toasts.svelte';
  import { dialogs } from '../lib/dialogs.svelte';
  import { href, plainClick, route } from '../lib/route.svelte';
  import { mirror } from '../lib/mirror.svelte';
  import { t } from '../lib/i18n.svelte';
  import type { ModSummary, PackSummary, ServerEntry, UnassignedJar } from '../lib/types';
  import ServerEditor from './ServerEditor.svelte';
  import PackEditor from './PackEditor.svelte';
  import UsersView from './UsersView.svelte';
  import Moderation from './Moderation.svelte';
  import Audit from './Audit.svelte';
  import DataTable, { type Column } from './ui/DataTable.svelte';

  let { me }: { me: { login: string } } = $props();

  // the active section comes from the shared route store; the shell rail drives it

  // server create/edit: 'new' = creating, ServerEntry = editing, null = closed
  let serverEdit = $state<ServerEntry | 'new' | null>(null);
  // pack editor: pack_id being edited, null = closed
  // the open editor is a location now (route.pack), not local state -- see #54

  let packs = $state<PackSummary[]>([]);
  let servers = $state<ServerEntry[]>([]);
  let mods = $state<ModSummary[]>([]);
  let unassigned = $state<UnassignedJar[]>([]);
  let removed = $state<string[]>([]);
  let authoring = $state<string[]>([]);
  let cacheBytes = $state(0);
  let cacheCount = $state(0);
  let loading = $state(true);

  async function loadAll() {
    loading = true;
    try {
      const [p, s, md, u, a, rm, ci] = await Promise.all([
        api.adminSummaries(),
        api.servers(),
        api.registryMods(),
        api.unassigned(),
        api.authoringPacks(),
        api.removed(),
        api.cacheInventory(),
      ]);
      packs = p;
      servers = s.servers;
      mods = md.rows;
      unassigned = u;
      authoring = a.packs;
      removed = rm.removed;
      cacheBytes = ci.entries.reduce((n, e) => n + e.size_bytes, 0);
      cacheCount = ci.entries.length;
    } catch (e) {
      notifyFail(e);
    } finally {
      loading = false;
    }
  }
  loadAll();

  // This view is the whole mirror at a glance -- the catalog, the registry and
  // the cache -- so any of those changing makes it stale. Settled rather than
  // reloaded on each event: one read here is seven requests, three of which
  // answer whole collections (every mod the registry knows, every jar in the
  // cache, and the diff between them), and a harvest landing or a burst of
  // publishes fires several events in a row. A short quiet window turns a burst
  // into one refresh; a single event still lands in well under a second.
  let settle: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    if (mirror.packs + mirror.registry + mirror.catalog === 0) return;
    clearTimeout(settle);
    settle = setTimeout(() => void loadAll(), 400);
    return () => clearTimeout(settle);
  });

  // The list behind the editor is stale the moment editing ends, and editing now
  // ends in more ways than one button -- back and the trackpad gesture close it
  // too. Refresh on the close itself rather than from whatever triggered it.
  let editorWasOpen = false;
  $effect(() => {
    const open = route.pack !== null;
    if (editorWasOpen && !open) loadAll();
    editorWasOpen = open;
  });

  async function delServer(id: string) {
    const ok = await dialogs.confirm(t('servers.deleteMsg', { id }), {
      title: t('servers.deleteTitle'),
      danger: true,
    });
    if (!ok) return;
    try {
      await api.deleteServer(id);
      await loadAll();
    } catch (e) {
      notifyFail(e);
    }
  }

  // publish/unpublish a built pack: flips visibility between draft and published.
  // Takes effect on the public listing immediately (the mirror patches summary.json).
  async function togglePublish(p: PackSummary) {
    const next = p.visibility === 'published' ? 'draft' : 'published';
    try {
      await api.setVisibility(p.pack_id, next);
      await loadAll();
    } catch (e) {
      notifyFail(e);
    }
  }

  // typed lookup so t() gets a literal MsgKey, not a dynamic string
  const visKey = {
    draft: 'packs.vis.draft',
    unlisted: 'packs.vis.unlisted',
    published: 'packs.vis.published',
  } as const;

  const authoringSet = $derived(new Set(authoring));
  const allPackIds = $derived(
    [...new Set([...packs.map((p) => p.pack_id), ...authoring])].sort(),
  );
  const summaryFor = (id: string) => packs.find((p) => p.pack_id === id);

  // Row objects the packs table sorts and filters over. The summary is carried
  // whole so the row snippet renders exactly what the hand-rolled table did.
  type PackRow = {
    id: string;
    summary: PackSummary | undefined;
    name: string;
    mc: string;
    latest: string;
    visibility: string;
    authoring: boolean;
  };
  const packRows = $derived<PackRow[]>(
    allPackIds.map((id) => {
      const p = summaryFor(id);
      return {
        id,
        summary: p,
        name: p?.display_name ?? id,
        mc: p?.minecraft_version ?? '',
        latest: p?.latest_pack_version ?? '',
        visibility: p?.visibility ?? '',
        authoring: authoringSet.has(id),
      };
    }),
  );
  let packFilter = $state('');
  const packColumns = $derived<Column<PackRow>[]>([
    { id: 'pack', header: t('packs.col.pack'), sortable: true, value: (r) => r.name },
    { id: 'mc', header: t('packs.col.mc'), sortable: true, value: (r) => r.mc },
    { id: 'latest', header: t('packs.col.latest'), sortable: true, value: (r) => r.latest },
    { id: 'state', header: t('packs.col.state'), sortable: true, value: (r) => r.visibility },
    { id: 'tags', header: t('packs.col.tags') },
    { id: 'flags', header: t('packs.col.flags') },
    { id: 'actions', header: '', width: '90px' },
  ]);

  const serverColumns = $derived<Column<ServerEntry>[]>([
    { id: 'server', header: t('servers.col.server'), sortable: true, value: (s) => s.display_name },
    { id: 'pack', header: t('packs.col.pack'), sortable: true, value: (s) => s.pack_id },
    { id: 'owner', header: t('servers.col.owner'), sortable: true, value: (s) => s.owner_display },
    { id: 'flags', header: t('packs.col.flags') },
    { id: 'actions', header: '', width: '90px' },
  ]);

  // overview metrics
  const unbuiltCount = $derived(allPackIds.filter((id) => !summaryFor(id)).length);
  const builtCount = $derived(allPackIds.length - unbuiltCount);
  const featPackCount = $derived(packs.filter((p) => p.featured).length);
  const featServerCount = $derived(servers.filter((s) => s.featured).length);

  // Recent builds: built packs, newest first by when the build was actually
  // made. That is `latest_built_at`, which the mirror derives from the
  // manifest's own timestamp -- this used to dig a YYYY.MM.DD out of the
  // version string, which no version has carried since numbering became
  // `<base>.<counter>`, so every entry scored the empty string and the list was
  // in whatever order the catalog happened to arrive in. A pack whose manifest
  // cannot be read has no date and sorts last rather than first.
  const recentBuilds = $derived(
    packs
      .filter((p) => p.latest_pack_version)
      .map((p) => ({
        pack: p.display_name,
        ver: p.latest_pack_version,
        at: p.latest_built_at ?? '',
        date: p.latest_built_at?.slice(0, 10) ?? '',
      }))
      .sort((a, b) => b.at.localeCompare(a.at)),
  );
  const cacheNum = $derived(
    cacheBytes >= 1e9
      ? (cacheBytes / 1e9).toFixed(1)
      : cacheBytes >= 1e6
        ? (cacheBytes / 1e6).toFixed(0)
        : `${Math.max(1, Math.round(cacheBytes / 1e3))}`,
  );
  const cacheUnit = $derived(cacheBytes >= 1e9 ? 'GB' : cacheBytes >= 1e6 ? 'MB' : 'KB');

  async function newPack() {
    const id = (
      await dialogs.prompt(t('packs.newPrompt'), { title: t('packs.new') })
    )?.trim();
    if (id) route.openPack(id);
  }
</script>

<div class="view">

  <div class="body">
    {#if route.section === 'overview'}
      <section class="ov">
        <div class="seclabel">{t('overview.status')}</div>
        <div class="readout">
          <div class="stat">
            <div class="k">{t('overview.packs')}</div>
            <div class="v" use:countUp={allPackIds.length}></div>
            <div class="s">{t('overview.packsSub', { built: builtCount, unbuilt: unbuiltCount })}</div>
          </div>
          <div class="stat">
            <div class="k">{t('mm.overviewMods')}</div>
            <div class="v" use:countUp={mods.length}></div>
            <div class="s">
              {unassigned.length
                ? t('mm.overviewModsSub', { n: unassigned.length })
                : `${t('overview.authoring')}: ${authoring.length}`}
            </div>
          </div>
          <div class="stat">
            <div class="k">{t('overview.cache')}</div>
            <div class="v">{cacheNum}<small class="unit">{cacheUnit}</small></div>
            <div class="s">{cacheCount} jar</div>
          </div>
          <div class="stat">
            <div class="k">{#if removed.length}<span class="d"></span>{/if}{t('overview.takedown')}</div>
            <div class="v" use:countUp={removed.length}></div>
            <div class="s">blocked sha1</div>
          </div>
        </div>

        <div class="cols">
          <div class="card">
            <h3>{t('overview.packs')}</h3>
            {#each allPackIds as id}
              {@const p = summaryFor(id)}
              <a
                class="lrow clickable"
                href={href.pack(id, 'packs')}
                onclick={(e) => {
                  if (!plainClick(e)) return;
                  e.preventDefault();
                  route.openPackIn('packs', id);
                }}
              >
                <span class="av">{(p?.display_name ?? id).slice(0, 2).toUpperCase()}</span>
                <div class="lcol">
                  <div class="nm">{p?.display_name ?? id}</div>
                  <div class="mm">
                    {p?.minecraft_version ?? '-'} &middot; {p?.latest_pack_version ?? t('packs.unbuilt')}
                  </div>
                </div>
                <div class="grow"></div>
                {#if p}
                  <span class="chip ok"><span class="g"></span>built</span>
                {:else}
                  <span class="chip"><span class="g"></span>{t('packs.unbuilt')}</span>
                {/if}
              </a>
            {/each}
            {#if allPackIds.length === 0 && !loading}
              <div class="lrow"><span class="muted">{t('packs.empty')}</span></div>
            {/if}
          </div>

          <div class="card">
            <h3>{t('overview.recent')}</h3>
            {#each recentBuilds as b}
              <div class="frow">
                <span class="ft">{b.date || '—'}</span>
                <span class="fx"><b>{b.pack}</b> {t('overview.built')} &middot; <span class="mono">{b.ver}</span></span>
              </div>
            {/each}
            {#if recentBuilds.length === 0 && !loading}
              <div class="frow"><span class="muted">{t('overview.noBuilds')}</span></div>
            {/if}
          </div>
        </div>

        <div class="seclabel">{t('overview.controls')}</div>
        <div class="controls">
          <button class="primary" onclick={newPack}>{t('packs.new')}</button>
        </div>
      </section>
    {:else if route.section === 'packs'}
      {#if route.pack !== null}
        {#key route.pack}
          <PackEditor
            packId={route.pack}
            {me}
            onClose={() => route.closePack()}
          />
        {/key}
      {:else}
        <div class="bar">
          <button class="primary" onclick={newPack}>{t('packs.new')}</button>
          <input class="tfilter mono" bind:value={packFilter} placeholder={t('packs.filter')} aria-label={t('packs.filter')} />
        </div>
        <div class="panel">
          <DataTable data={packRows} columns={packColumns} filter={packFilter} row={packRow} empty={packEmpty} />
        </div>
        {#snippet packRow(r: PackRow)}
          <tr
            class="clickable"
            role="button"
            tabindex="0"
            onclick={() => route.openPack(r.id)}
            onkeydown={(e) => {
              if (e.target !== e.currentTarget) return;
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                route.openPack(r.id);
              }
            }}
          >
            <td>
              <!-- the row stays clickable for the mouse, but the name is the real
                   destination: a link the browser can open, copy and announce -->
              <a
                class="rowlink"
                href={href.pack(r.id, 'packs')}
                onclick={(e) => {
                  if (!plainClick(e)) return;
                  e.preventDefault();
                  route.openPack(r.id);
                }}>{r.name}</a>
              {#if r.name !== r.id}<div class="faint mono">{r.id}</div>{/if}
            </td>
            <td class="mono">{r.summary?.minecraft_version ?? '-'}</td>
            <td class="mono">{r.summary?.latest_pack_version ?? t('packs.unbuilt')}</td>
            <td>
              {#if r.summary}
                <span class="tag vis-{r.summary.visibility}">{t(visKey[r.summary.visibility])}</span>
                {#if r.summary.tier === 'community'}<span class="tag">{t('packs.tier.community')}</span>{/if}
              {/if}
            </td>
            <td>{#each r.summary?.tags ?? [] as tg}<span class="tag">{tg}</span> {/each}</td>
            <td>
              {#if r.summary?.featured}<span class="tag" style="color:var(--accent)">{t('packs.flag.featured')}</span>{/if}
              {#if r.authoring}<span class="tag" style="color:var(--ok)">{t('packs.flag.authoring')}</span>{/if}
            </td>
            <td class="actions">
              {#if r.summary}
                {@const sm = r.summary}
                <button
                  onclick={(e) => {
                    e.stopPropagation();
                    togglePublish(sm);
                  }}>{sm.visibility === 'published' ? t('packs.unpublish') : t('packs.publish')}</button>
              {/if}
            </td>
          </tr>
        {/snippet}
        {#snippet packEmpty()}
          <tr><td colspan="7" class="muted">{t('packs.empty')}</td></tr>
        {/snippet}
      {/if}
    {:else if route.section === 'servers'}
      {#if serverEdit !== null}
        {#key serverEdit}
          <ServerEditor
            initial={serverEdit === 'new' ? null : serverEdit}
            packIds={packs.map((p) => p.pack_id)}
            onSaved={() => {
              serverEdit = null;
              loadAll();
            }}
            onCancel={() => (serverEdit = null)}
          />
        {/key}
      {:else}
        <div class="bar">
          <button class="primary" onclick={() => (serverEdit = 'new')}>{t('servers.new')}</button>
        </div>
        <div class="panel">
          <DataTable data={servers} columns={serverColumns} row={serverRow} empty={serverEmpty} />
        </div>
        {#snippet serverRow(s: ServerEntry)}
          <tr
            class="clickable"
            role="button"
            tabindex="0"
            onclick={() => (serverEdit = s)}
            onkeydown={(e) => {
              if (e.target !== e.currentTarget) return;
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                serverEdit = s;
              }
            }}
          >
            <td>
              <div>{s.display_name}</div>
              <div class="faint mono">{s.server_id}</div>
            </td>
            <td class="mono">{s.pack_id}</td>
            <td>{s.owner_display}</td>
            <td>
              {#if s.featured}<span class="tag" style="color:var(--accent)">{t('packs.flag.featured')}</span>{/if}
            </td>
            <td class="actions">
              <button
                class="danger"
                onclick={(e) => {
                  e.stopPropagation();
                  delServer(s.server_id);
                }}>{t('common.delete')}</button>
            </td>
          </tr>
        {/snippet}
        {#snippet serverEmpty()}
          <tr><td colspan="5" class="muted">{t('servers.empty')}</td></tr>
        {/snippet}
      {/if}
    {:else if route.section === 'users'}
      <UsersView />
    {:else if route.section === 'moderation'}
      <Moderation />
    {:else if route.section === 'audit'}
      <Audit />
    {/if}
  </div>
</div>

<style>
  .view {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .body {
    min-width: 0;
  }
  tr.clickable {
    cursor: pointer;
  }
  tr.clickable:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .err {
    color: var(--danger);
    background: var(--danger-soft);
    border: 1px solid color-mix(in srgb, var(--danger) 40%, transparent);
    border-radius: var(--radius-sm);
    padding: var(--space-3) var(--space-4);
    font-size: var(--fs-sm);
  }
  .ov {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .readout {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(158px, 1fr));
    border: 1px solid var(--seam);
    border-radius: var(--radius-md);
    overflow: hidden;
    background: var(--panel);
    box-shadow: var(--shadow-1);
  }
  .stat {
    padding: var(--space-4);
    border-right: 1px solid var(--seam);
  }
  .stat:last-child {
    border-right: none;
  }
  .stat .k {
    font-size: var(--fs-sm);
    color: var(--fg-dim);
    display: flex;
    align-items: center;
    gap: 7px;
  }
  .stat .k .d {
    width: 6px;
    height: 6px;
    border-radius: 999px;
    background: var(--danger);
  }
  .stat .v {
    font-family: var(--mono);
    font-size: var(--fs-3xl);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    letter-spacing: 0;
    margin-top: 6px;
  }
  .stat .v .unit {
    font-size: var(--fs-lg);
    font-weight: 600;
    color: var(--fg-dim);
    margin-left: 3px;
  }
  .stat .s {
    font-size: var(--fs-xs);
    color: var(--fg-faint);
    margin-top: 3px;
  }
  .cols {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--space-4);
  }
  @container view (max-width: 768px) {
    .cols {
      grid-template-columns: 1fr;
    }
  }
  .card {
    border: 1px solid var(--seam);
    border-radius: var(--radius-md);
    background: var(--panel);
    overflow: hidden;
    box-shadow: var(--shadow-1);
  }
  .card h3 {
    font-family: var(--mono);
    font-size: var(--fs-xs);
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--fg-dim);
    margin: 0;
    padding: var(--space-3) var(--space-4);
    border-bottom: 1px solid var(--seam);
  }
  .rowlink {
    color: inherit;
    text-decoration: none;
  }
  .rowlink:hover {
    text-decoration: underline;
    text-decoration-color: var(--seam-bright);
  }
  .lrow {
    display: flex;
    color: inherit;
    text-decoration: none;
    align-items: center;
    gap: var(--space-3);
    padding: 11px var(--space-4);
    border-bottom: 1px solid var(--seam);
  }
  .lrow:last-child {
    border-bottom: none;
  }
  .lrow.clickable {
    cursor: pointer;
  }
  .lrow.clickable:hover {
    background: var(--panel-2);
  }
  .lrow.clickable:focus-visible {
    outline: 2px solid var(--fg);
    outline-offset: -2px;
  }
  .lrow .av {
    width: 30px;
    height: 30px;
    border-radius: 8px;
    background: var(--panel-3);
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: var(--fs-xs);
    font-weight: 700;
    color: var(--fg-dim);
    flex: none;
  }
  .lcol {
    min-width: 0;
  }
  .lrow .nm {
    font-weight: 600;
    font-size: var(--fs-md);
  }
  .lrow .mm {
    font-family: var(--mono);
    font-size: var(--fs-xs);
    color: var(--fg-faint);
  }
  .lrow .grow {
    flex: 1;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    white-space: nowrap;
    font-size: var(--fs-xs);
    font-weight: 600;
    padding: 3px 10px;
    border-radius: 999px;
    background: var(--panel-2);
    color: var(--fg-dim);
  }
  .chip .g {
    width: 6px;
    height: 6px;
    border-radius: 999px;
    background: var(--fg-faint);
  }
  .chip.ok {
    color: var(--ok);
    background: var(--ok-soft);
  }
  .chip.ok .g {
    background: var(--ok);
  }
  .seclabel {
    font-family: var(--mono);
    font-size: var(--fs-xs);
    font-weight: 600;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--fg-faint);
    margin-bottom: -4px;
  }
  .frow {
    display: flex;
    align-items: baseline;
    gap: var(--space-3);
    padding: 10px var(--space-4);
    border-bottom: 1px solid var(--seam);
    font-size: var(--fs-md);
  }
  .frow:last-child {
    border-bottom: none;
  }
  .frow .ft {
    color: var(--fg-faint);
    font-size: var(--fs-xs);
    white-space: nowrap;
    font-family: var(--mono);
  }
  .frow .fx {
    color: var(--fg-dim);
  }
  .frow .fx b {
    color: var(--fg);
    font-weight: 600;
  }
  .controls {
    display: flex;
    gap: var(--space-2);
    flex-wrap: wrap;
    align-items: center;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    margin-bottom: 14px;
  }
  .tfilter {
    width: 240px;
    max-width: 100%;
    padding: 7px 11px;
    font-size: var(--fs-sm);
  }
  .actions {
    white-space: nowrap;
  }
  .actions button {
    padding: 4px 10px;
    font-size: var(--fs-sm);
    margin-right: 6px;
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
</style>
