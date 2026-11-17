<script lang="ts">
  import { api } from '../lib/api';
  import { countUp } from '../lib/motion.svelte';
  import { notifyFail, toasts } from '../lib/toasts.svelte';
  import { idError, say } from '../lib/validate';
  import { dialogs } from '../lib/dialogs.svelte';
  import { ADMIN_TABS, href, plainClick, route, type AdminTab } from '../lib/route.svelte';
  import { mirror } from '../lib/mirror.svelte';
  import { i18n, t } from '../lib/i18n.svelte';
  import type { AuditRow, ModSummary, PackSummary, Thread, UnassignedJar, UploadRow } from '../lib/types';
  import PackEditor from './PackEditor.svelte';
  import UsersView from './UsersView.svelte';
  import Moderation from './Moderation.svelte';
  import Audit from './Audit.svelte';
  import DataTable, { type Column } from './ui/DataTable.svelte';
  import PackThumb from './ui/PackThumb.svelte';
  import TabStrip from './ui/TabStrip.svelte';
  import Glyph from './ui/Glyph.svelte';

  let { me }: { me: { login: string } } = $props();

  // the active section comes from the shared route store; the shell rail drives it
  // the open editor is a location (route.pack), not local state -- see #54

  let packs = $state<PackSummary[]>([]);
  let mods = $state<ModSummary[]>([]);
  let unassigned = $state<UnassignedJar[]>([]);
  let removed = $state<string[]>([]);
  let authoring = $state<string[]>([]);
  let cacheBytes = $state(0);
  let cacheCount = $state(0);
  let pending = $state<UploadRow[]>([]);
  let loading = $state(true);

  async function loadAll() {
    loading = true;
    try {
      const [p, md, u, a, rm, ci, up] = await Promise.all([
        api.adminSummaries(),
        api.registryMods(),
        api.unassigned(),
        api.authoringPacks(),
        api.removed(),
        api.cacheInventory(),
        api.pendingUploads(),
      ]);
      packs = p;
      mods = md.rows;
      unassigned = u;
      authoring = a.packs;
      removed = rm.removed;
      cacheBytes = ci.entries.reduce((n, e) => n + e.size_bytes, 0);
      cacheCount = ci.entries.length;
      pending = up;
    } catch (e) {
      notifyFail(e);
    } finally {
      loading = false;
    }
  }
  loadAll();

  // This view is the whole mirror at a glance -- the catalog, the registry and
  // the cache -- so any of those changing makes it stale. Settled rather than
  // reloaded on each event: one read here is several requests, three of which
  // answer whole collections (every mod the registry knows, every jar in the
  // cache, and the diff between them), and a harvest landing or a burst of
  // publishes fires several events in a row. A short quiet window turns a burst
  // into one refresh; a single event still lands in well under a second.
  let quiet: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    if (mirror.packs + mirror.registry + mirror.catalog + mirror.moderation === 0) return;
    clearTimeout(quiet);
    quiet = setTimeout(() => void loadAll(), 400);
    return () => clearTimeout(quiet);
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

  // Official is the mirror vouching for a pack: it moves the pack from the
  // community listing into the launcher's catalog, so it is asked first.
  async function toggleTier(p: PackSummary) {
    const next = p.tier === 'official' ? 'community' : 'official';
    const ok = await dialogs.confirm(
      t(next === 'official' ? 'packs.makeOfficialAsk' : 'packs.makePersonalAsk', { name: p.display_name }),
      { title: t(next === 'official' ? 'packs.makeOfficial' : 'packs.makePersonal') },
    );
    if (!ok) return;
    try {
      await api.setTier(p.pack_id, next);
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
    { id: 'actions', header: '', width: '240px' },
  ]);

  // ── the overview ──
  //
  // What waits on the operator first, then what happened lately. The catalog
  // itself is the packs section's to list: an overview that repeats it is a
  // second copy of a table, not a view of the mirror.

  // Open discussions across every pack, newest first, read only while the
  // overview is on screen: it is a request per pack, and no other view needs it.
  let talk = $state<Thread[]>([]);
  let trail = $state<AuditRow[]>([]);
  async function loadAttention() {
    const ids = allPackIds;
    const [threads, audit] = await Promise.all([
      // a whole page per pack, so the tile counts what is open rather than
      // the first few of each
      Promise.allSettled(ids.map((id) => api.threads(id, undefined, false, 50))),
      api.auditLog(8).catch(() => null),
    ]);
    talk = threads
      .flatMap((r) => (r.status === 'fulfilled' ? r.value.rows : []))
      .sort((a, b) => b.created_at - a.created_at);
    trail = audit?.rows ?? [];
  }
  $effect(() => {
    if (route.section !== 'overview' || !allPackIds.length) return;
    void mirror.packs;
    void loadAttention();
  });

  // Built packs, newest build first. `latest_built_at` is the manifest's own
  // timestamp; a pack whose manifest cannot be read has no date and is left out
  // of a list that is about when.
  const recentBuilds = $derived(
    packs
      .filter((p) => p.latest_built_at)
      .sort((a, b) => (b.latest_built_at ?? '').localeCompare(a.latest_built_at ?? ''))
      .slice(0, 6),
  );

  const cacheSize = $derived(
    cacheBytes >= 1e9
      ? `${(cacheBytes / 1e9).toFixed(1)} ${t('catalog.gb')}`
      : cacheBytes >= 1e6
        ? `${Math.round(cacheBytes / 1e6)} ${t('catalog.mb')}`
        : `${Math.max(1, Math.round(cacheBytes / 1e3))} ${t('catalog.kb')}`,
  );

  /// "3 hours ago", in the reader's language, for something that happened at
  /// `ms`. A date says when; this says how fresh, which is what an overview is
  /// asked.
  function ago(ms: number): string {
    const rtf = new Intl.RelativeTimeFormat(i18n.locale, { numeric: 'auto' });
    const s = Math.round((ms - Date.now()) / 1000);
    const steps: [Intl.RelativeTimeFormatUnit, number][] = [
      ['year', 31536000],
      ['month', 2592000],
      ['week', 604800],
      ['day', 86400],
      ['hour', 3600],
      ['minute', 60],
    ];
    for (const [unit, size] of steps) {
      if (Math.abs(s) >= size) return rtf.format(Math.round(s / size), unit);
    }
    return rtf.format(0, 'minute');
  }

  const kindKey = { issue: 'thr.kind.issue', topic: 'thr.kind.topic', proposal: 'thr.kind.proposal' } as const;
  function kindOf(k: string): string {
    return k in kindKey ? t(kindKey[k as keyof typeof kindKey]) : k;
  }
  const packName = (id: string) => summaryFor(id)?.display_name ?? id;

  // ── administration ──
  const adminLabel: Record<AdminTab, 'nav.users' | 'nav.moderation' | 'nav.audit'> = {
    users: 'nav.users',
    moderation: 'nav.moderation',
    audit: 'nav.audit',
  };
  const adminTabs = $derived(
    ADMIN_TABS.map((tab) => ({
      value: tab,
      label: tab === 'moderation' && pending.length ? `${t(adminLabel[tab])} ${pending.length}` : t(adminLabel[tab]),
    })),
  );

  // Opened into the packs section rather than wherever the button was pressed.
  // The editor is a location (#54), and only `/packs/<id>` and `/mypacks/<id>`
  // are locations the router can read back.
  //
  // The id is checked here for the same reason the editor's own field checks
  // it: a name the mirror will refuse should be refused before somebody fills
  // an editor with work behind it.
  async function newPack() {
    const id = (
      await dialogs.prompt(t('packs.newPrompt'), { title: t('packs.new') })
    )?.trim();
    if (!id) return;
    const bad = say(idError(id));
    if (bad) {
      toasts.push({ kind: 'error', text: bad });
      return;
    }
    route.openPackIn('packs', id);
  }
</script>

<div class="view">

  <div class="body">
    {#if route.section === 'overview'}
      <section class="ov">
        <!-- what waits on somebody, each a way to where it is dealt with -->
        <div class="tiles">
          <a
            class="tile"
            class:hot={pending.length > 0}
            href={href.admin('moderation')}
            onclick={(e) => {
              if (!plainClick(e)) return;
              e.preventDefault();
              void route.goAdmin('moderation');
            }}
          >
            <span class="tk">{t('ov.pending')}</span>
            <span class="tv" use:countUp={pending.length}></span>
            <span class="ts">{pending.length ? t('ov.pendingSub') : t('ov.nothingWaits')}</span>
          </a>
          <div class="tile" class:hot={talk.length > 0}>
            <span class="tk">{t('ov.talk')}</span>
            <span class="tv" use:countUp={talk.length}></span>
            <span class="ts">{t('ov.talkSub')}</span>
          </div>
          <a
            class="tile"
            class:hot={unassigned.length > 0}
            href={href.section('mods')}
            onclick={(e) => {
              if (!plainClick(e)) return;
              e.preventDefault();
              route.go('mods');
            }}
          >
            <span class="tk">{t('ov.unknownJars')}</span>
            <span class="tv" use:countUp={unassigned.length}></span>
            <span class="ts">{t('ov.unknownJarsSub', { n: mods.length })}</span>
          </a>
          <div class="tile">
            <span class="tk">{t('overview.cache')}</span>
            <span class="tv">{cacheSize}</span>
            <span class="ts">{t('ov.cacheSub', { n: cacheCount })}</span>
          </div>
          <div class="tile">
            <span class="tk">{t('overview.takedown')}</span>
            <span class="tv" use:countUp={removed.length}></span>
            <span class="ts">{t('ov.takedownSub')}</span>
          </div>
        </div>

        {#if talk.length}
          <div class="card">
            <h3>{t('ov.talkTitle')}</h3>
            {#each talk.slice(0, 6) as th (th.id)}
              <a
                class="lrow"
                href={href.discussion(th.id)}
                onclick={(e) => {
                  if (!plainClick(e)) return;
                  e.preventDefault();
                  route.readThread(th.id);
                }}
              >
                <span class="badge"><Glyph name={th.kind === 'issue' ? 'alert' : th.kind === 'proposal' ? 'fork' : 'chat'} size={14} /></span>
                <span class="lcol">
                  <span class="nm">{th.title}</span>
                  <span class="mm">{packName(th.pack_id)} &middot; {kindOf(th.kind)} &middot; {th.by_login ?? th.by_uid}</span>
                </span>
                <span class="when">{ago(th.created_at * 1000)}</span>
              </a>
            {/each}
          </div>

        {/if}

        <div class="cols">
          <div class="card">
            <h3>{t('overview.recent')}</h3>
            {#each recentBuilds as p (p.pack_id)}
              <a
                class="lrow"
                href={href.pack(p.pack_id, 'packs')}
                onclick={(e) => {
                  if (!plainClick(e)) return;
                  e.preventDefault();
                  route.openPackIn('packs', p.pack_id);
                }}
              >
                <PackThumb name={p.display_name} banner={p.banner_url} icon={p.icon_url} />
                <span class="lcol">
                  <span class="nm">{p.display_name}</span>
                  <span class="mm mono">{p.latest_pack_version}{#if p.latest_channel}&nbsp;&middot; {t(`catalog.ch.${p.latest_channel}` as 'catalog.ch.release')}{/if}</span>
                </span>
                <span class="when">{ago(Date.parse(p.latest_built_at ?? ''))}</span>
              </a>
            {/each}
            {#if !recentBuilds.length && !loading}
              <p class="none">{t('overview.noBuilds')}</p>
            {/if}
          </div>
          <div class="card">
            <h3>
              {t('ov.trail')}
              <a
                class="more"
                href={href.admin('audit')}
                onclick={(e) => {
                  if (!plainClick(e)) return;
                  e.preventDefault();
                  void route.goAdmin('audit');
                }}>{t('ov.trailAll')}</a
              >
            </h3>
            {#each trail as r (r.id)}
              <div class="arow">
                <span class="act mono">{r.action}</span>
                <span class="who">{r.actor_login}</span>
                <span class="tgt mono">{r.target ?? ''}{#if r.detail}&nbsp;&middot; {r.detail}{/if}</span>
                <span class="when">{ago(r.created_at * 1000)}</span>
              </div>
            {/each}
            {#if !trail.length}
              <p class="none">{t('audit.empty')}</p>
            {/if}
          </div>
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
          <DataTable data={packRows} columns={packColumns} filter={packFilter} row={packRow} onRow={(r) => route.openPack(r.id)} empty={packEmpty} />
        </div>
        {#snippet packRow(r: PackRow)}
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
                <span class="tag" class:personal={r.summary.tier === 'community'}>{r.summary.tier === 'community' ? t('packs.tier.personal') : t('packs.tier.official')}</span>
              {/if}
            </td>
            <td class="narrow-hide">{#each r.summary?.tags ?? [] as tg}<span class="tag">{tg}</span> {/each}</td>
            <td class="narrow-hide">
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
                <button
                  onclick={(e) => {
                    e.stopPropagation();
                    toggleTier(sm);
                  }}>{sm.tier === 'official' ? t('packs.makePersonal') : t('packs.makeOfficial')}</button>
              {/if}
            </td>
        {/snippet}
        {#snippet packEmpty()}
          <tr><td colspan="7" class="muted">{t('packs.empty')}</td></tr>
        {/snippet}
      {/if}
    {:else if route.section === 'admin'}
      <div class="admin">
        <TabStrip
          value={route.adminTab}
          tabs={adminTabs}
          ariaLabel={t('nav.admin')}
          onChange={(v) => route.openAdmin(v as AdminTab)}
        />
        {#if route.adminTab === 'users'}
          <UsersView />
        {:else if route.adminTab === 'moderation'}
          <Moderation />
        {:else}
          <Audit />
        {/if}
      </div>
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
  .ov {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }

  /* What waits on the operator. A tile that leads somewhere is a link and
     reads as one under the pointer; a tile with something waiting in it wears
     the accent on its edge, so the eye goes there before reading a number. */
  .tiles {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    gap: var(--space-3);
  }
  .tile {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: var(--space-4);
    border: 1px solid var(--seam);
    border-radius: var(--radius-md);
    background: var(--panel);
    box-shadow: var(--shadow-1);
    color: inherit;
    text-decoration: none;
    transition:
      border-color var(--dur-state) var(--ease-out),
      background var(--dur-state) var(--ease-out);
  }
  a.tile:hover {
    border-color: var(--seam-bright);
    background: var(--panel-2);
  }
  a.tile:focus-visible {
    outline: none;
    box-shadow: var(--ring);
  }
  .tile.hot {
    box-shadow:
      inset 3px 0 0 var(--accent-hue),
      var(--shadow-1);
  }
  .tk {
    font-size: var(--fs-sm);
    color: var(--fg-dim);
  }
  .tv {
    font-family: var(--mono);
    font-size: var(--fs-3xl);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    line-height: 1.1;
  }
  .ts {
    font-size: var(--fs-xs);
    color: var(--fg-faint);
  }

  .cols {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: var(--space-4);
    align-items: start;
  }
  @container view (max-width: 768px) {
    .cols {
      grid-template-columns: minmax(0, 1fr);
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
    display: flex;
    align-items: baseline;
    gap: var(--space-3);
    margin: 0;
    padding: var(--space-3) var(--space-4);
    border-bottom: 1px solid var(--seam);
    font-size: var(--fs-md);
    font-weight: 650;
  }
  .card h3 .more {
    margin-left: auto;
    font-size: var(--fs-sm);
    font-weight: 400;
    color: var(--fg-dim);
  }
  .none {
    margin: 0;
    padding: var(--space-4);
    font-size: var(--fs-sm);
    color: var(--fg-dim);
  }
  .lrow {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: 10px var(--space-4);
    border-bottom: 1px solid var(--seam);
    color: inherit;
    text-decoration: none;
    transition: background var(--dur-state) var(--ease-out);
  }
  .lrow:last-child {
    border-bottom: none;
  }
  .lrow:hover {
    background: var(--panel-2);
  }
  .lrow:focus-visible {
    outline: none;
    box-shadow: inset 0 0 0 2px var(--seam-bright);
  }
  .lcol {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .nm {
    font-weight: 600;
    font-size: var(--fs-md);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .mm {
    font-size: var(--fs-xs);
    color: var(--fg-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badge {
    width: 26px;
    height: 26px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    background: var(--panel-2);
    color: var(--fg-dim);
  }
  .when {
    flex: none;
    font-size: var(--fs-xs);
    color: var(--fg-faint);
    white-space: nowrap;
  }
  /* the trail: what was done and by whom, how long ago, and to what under it */
  .arow {
    display: grid;
    grid-template-columns: 14ch minmax(0, 1fr) auto;
    grid-template-areas:
      'act who when'
      'tgt tgt tgt';
    align-items: baseline;
    gap: 2px var(--space-3);
    padding: 8px var(--space-4);
    border-bottom: 1px solid var(--seam);
    font-size: var(--fs-sm);
  }
  .arow .act {
    grid-area: act;
  }
  .arow .who {
    grid-area: who;
  }
  .arow .when {
    grid-area: when;
  }
  .arow .tgt {
    grid-area: tgt;
  }
  .arow:last-of-type {
    border-bottom: none;
  }
  .arow .act {
    font-size: var(--fs-xs);
    color: var(--fg-dim);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .arow .who {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .arow .tgt {
    font-size: var(--fs-xs);
    color: var(--fg-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .admin {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }

  .rowlink {
    color: inherit;
    text-decoration: none;
  }
  .rowlink:hover {
    text-decoration: underline;
    text-decoration-color: var(--seam-bright);
  }
  .bar {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    margin-bottom: 14px;
  }
  .bar .primary {
    flex: none;
    white-space: nowrap;
  }
  /* takes what the button leaves, up to its own width, rather than a fixed
     width that squeezed the button onto two lines on a phone */
  .tfilter {
    flex: 0 1 240px;
    min-width: 0;
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
  .tag.personal {
    color: var(--accent-hue);
  }
</style>
