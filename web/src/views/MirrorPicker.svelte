<script lang="ts">
  import { Dialog } from 'bits-ui';
  import { api, ApiError } from '../lib/api';
  import { t } from '../lib/i18n.svelte';
  import TabStrip from './ui/TabStrip.svelte';
  import { settle, stagger } from '../lib/motion.svelte';
  import type {
    ModSummary,
    VersionRow,
    BuildSummary,
    BuildModRow,
    CacheUsageEntry,
    DeclaredAsset,
    SourceDecl,
  } from '../lib/types';

  // a selection carries the resolved source -- a `smrt_cache` one when the mirror
  // holds the bytes, else a `modrinth` one -- plus a build mod's install flags
  type Sel = {
    filename: string;
    source: SourceDecl;
    required?: boolean;
    default_enabled?: boolean;
  };

  let {
    mc,
    loader,
    allowMany = false,
    present = [],
    onPick,
    onAddOne,
    onAddMany,
    onAddAsset,
    onClose,
  }: {
    mc?: string;
    loader?: string;
    // builds can re-add their whole mod set at once; only offered when adding a
    // new row (not when re-pointing an existing one)
    allowMany?: boolean;
    // source keys the pack already declares; those artifacts are shown but not
    // offered again (a pack ships one build of a mod)
    present?: string[];
    onPick: (sel: Sel) => void;
    // cherry-pick one mod from a build without closing the picker
    onAddOne?: (sel: Sel) => void;
    onAddMany?: (items: Sel[]) => void;
    // pull a build's assets into the pack (only when adding, not re-pointing)
    onAddAsset?: (a: DeclaredAsset) => void;
    onClose: () => void;
  } = $props();

  // how an artifact would be re-added: cache when the mirror holds the bytes,
  // else Modrinth; null when neither (a manifest-only mod whose Modrinth identity
  // didn't resolve -- not re-addable)
  type Artifact = {
    sha1: string;
    cached: boolean;
    modrinth_project_id: string | null;
    modrinth_version_id: string | null;
  };
  function sourceFor(a: Artifact): SourceDecl | null {
    if (a.cached) return { type: 'smrt_cache', sha1: a.sha1 };
    if (a.modrinth_project_id && a.modrinth_version_id)
      return { type: 'modrinth', project_id: a.modrinth_project_id, version_id: a.modrinth_version_id };
    return null;
  }
  const srcTag = (a: Artifact) =>
    a.cached ? 'cache' : a.modrinth_project_id && a.modrinth_version_id ? 'modrinth' : '';
  const presentSet = $derived(new Set(present));
  // the same identity the editor keys rows by: a cached jar by sha1, a Modrinth
  // artifact by project (not by pinned version -- another build of a mod already
  // in the pack is still that mod)
  function inPack(a: Artifact): boolean {
    const source = sourceFor(a);
    if (!source) return false;
    if (source.type === 'smrt_cache') return presentSet.has(`c:${source.sha1}`);
    if (source.type === 'modrinth') return presentSet.has(`m:${source.project_id}`);
    return false;
  }

  type Mode = 'mods' | 'builds' | 'raw';
  let mode = $state<Mode>('mods');
  let err = $state('');

  const modeTabs = $derived([
    { value: 'mods', label: t('mirror.tab.mods') },
    { value: 'builds', label: t('mirror.tab.builds') },
    { value: 'raw', label: t('mirror.tab.raw') },
  ]);

  // ── mods mode ──
  let q = $state('');
  // prefill the facet filters from the pack context; the picker is remounted on
  // each open, so capturing the initial prop value is the intended behaviour
  // svelte-ignore state_referenced_locally
  let loaderF = $state(loader ?? '');
  // svelte-ignore state_referenced_locally
  let mcF = $state(mc ?? '');
  let mods = $state<ModSummary[]>([]);
  let modsLoading = $state(false);
  let modTimer: ReturnType<typeof setTimeout> | undefined;
  // version step for a picked mod
  let selMod = $state<ModSummary | null>(null);
  let modVersions = $state<VersionRow[]>([]);
  let versLoading = $state(false);

  // ── builds mode ──
  let builds = $state<BuildSummary[]>([]);
  let buildsLoading = $state(false);
  let selBuild = $state<BuildSummary | null>(null);
  let buildModRows = $state<BuildModRow[]>([]);
  let buildAssetRows = $state<DeclaredAsset[]>([]);
  let bmLoading = $state(false);
  // sha1s / dests cherry-picked from the open build this session ("added" feedback)
  let addedShas = $state<Set<string>>(new Set());
  let addedDests = $state<Set<string>>(new Set());

  // ── raw cache mode ──
  let raw = $state<CacheUsageEntry[]>([]);
  let rawLoading = $state(false);
  let rawQ = $state('');

  function fail(e: unknown) {
    err = e instanceof ApiError ? `${e.status} ${e.body}` : String(e);
  }

  // Which search is the current one. The debounce shortens the window but does
  // not close it: a slow first request can land after a narrower second, and
  // then the list on screen does not match what is typed above it.
  let modsGeneration = 0;

  async function loadMods() {
    const mine = ++modsGeneration;
    modsLoading = true;
    err = '';
    try {
      const rows = (
        await api.registryMods(q.trim() || undefined, loaderF.trim() || undefined, mcF.trim() || undefined)
      ).rows;
      if (mine !== modsGeneration) return;
      mods = rows;
    } catch (e) {
      if (mine !== modsGeneration) return;
      fail(e);
    } finally {
      if (mine === modsGeneration) modsLoading = false;
    }
  }
  function onModFilter() {
    clearTimeout(modTimer);
    modTimer = setTimeout(loadMods, 250);
  }

  async function openMod(m: ModSummary) {
    selMod = m;
    modVersions = [];
    err = '';
    versLoading = true;
    try {
      modVersions = await api.registryModVersions(m.mod_id);
    } catch (e) {
      fail(e);
    } finally {
      versLoading = false;
    }
  }

  function pickVersion(v: VersionRow) {
    const source = sourceFor(v);
    if (!source) return;
    onPick({ filename: v.filename || `${selMod?.name ?? v.sha1.slice(0, 12)}.jar`, source });
  }

  async function loadBuilds() {
    buildsLoading = true;
    err = '';
    try {
      builds = await api.registryBuilds();
    } catch (e) {
      fail(e);
    } finally {
      buildsLoading = false;
    }
  }

  async function openBuild(b: BuildSummary) {
    selBuild = b;
    buildModRows = [];
    buildAssetRows = [];
    addedShas = new Set();
    addedDests = new Set();
    err = '';
    bmLoading = true;
    try {
      [buildModRows, buildAssetRows] = await Promise.all([
        api.registryBuildMods(b.pack_id, b.pack_version),
        api.registryBuildAssets(b.pack_id, b.pack_version),
      ]);
    } catch (e) {
      fail(e);
    } finally {
      bmLoading = false;
    }
  }

  const selOf = (m: BuildModRow, source: SourceDecl): Sel => ({
    filename: m.filename,
    source,
    default_enabled: m.default_enabled,
  });

  function addAllFromBuild() {
    if (!onAddMany) return;
    // skip mods whose artifact is neither cached nor Modrinth-resolvable
    const items = buildModRows
      .map((m) => {
        const source = sourceFor(m);
        return source && !inPack(m) ? selOf(m, source) : null;
      })
      .filter((x): x is Sel => x !== null);
    onAddMany(items);
  }

  // per-row Add on the Builds tab: when adding new rows, append and stay open so
  // the operator can pick several; when re-pointing one row, fall back to onPick
  // (which closes)
  function addBuildMod(m: BuildModRow) {
    const source = sourceFor(m);
    if (!source) return;
    if (allowMany && onAddOne) {
      onAddOne(selOf(m, source));
      addedShas = new Set(addedShas).add(m.sha1);
    } else {
      onPick(selOf(m, source));
    }
  }

  function addBuildAsset(a: DeclaredAsset) {
    if (!onAddAsset) return;
    onAddAsset(a);
    addedDests = new Set(addedDests).add(a.dest);
  }

  function addAllAssets() {
    if (!onAddAsset) return;
    for (const a of buildAssetRows) onAddAsset(a);
    addedDests = new Set(buildAssetRows.map((a) => a.dest));
  }

  async function loadRaw() {
    rawLoading = true;
    err = '';
    try {
      raw = (await api.cacheUsage()).entries;
    } catch (e) {
      fail(e);
    } finally {
      rawLoading = false;
    }
  }

  // Matched against every filename the jar is used under, not just the first: a
  // jar three packs ship under three names was only findable by the name the
  // first of them gave it.
  const rawShown = $derived(
    raw.filter((e) => {
      const n = rawQ.trim().toLowerCase();
      if (!n) return true;
      return (
        e.sha1.includes(n) || e.uses.some((u) => u.filename.toLowerCase().includes(n))
      );
    }),
  );
  const rawName = (e: CacheUsageEntry) => e.uses[0]?.filename ?? '';

  function setMode(m: Mode) {
    mode = m;
    selMod = null;
    selBuild = null;
    addedShas = new Set();
    addedDests = new Set();
    err = '';
    if (m === 'mods' && mods.length === 0) loadMods();
    if (m === 'builds' && builds.length === 0) loadBuilds();
    if (m === 'raw' && raw.length === 0) loadRaw();
  }

  function fmtBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    const u = ['KB', 'MB', 'GB'];
    let i = -1;
    do {
      n /= 1024;
      i++;
    } while (n >= 1024 && i < u.length - 1);
    return `${n.toFixed(1)} ${u[i]}`;
  }

  // initial load
  loadMods();

  // escape / outside-click flip Bits' open to false; the parent unmounts us on close
  function onOpenChange(open: boolean) {
    if (!open) onClose();
  }
</script>

<Dialog.Root open {onOpenChange}>
  <Dialog.Overlay class="dlg-scrim" />
  <Dialog.Content class="mirror-dlg panel">
    <Dialog.Title class="vh">{t('mirror.title')}</Dialog.Title>
    <div class="ph row">
      <TabStrip value={mode} tabs={modeTabs} ariaLabel={t('mirror.title')} onChange={(v) => setMode(v as Mode)} />
      <div class="spacer"></div>
      <button onclick={onClose}>{t('common.close')}</button>
    </div>

    {#if err}<div class="err mono">{err}</div>{/if}

    {#if mode === 'mods'}
      {#if !selMod}
        <div class="filters">
          <input class="grow" bind:value={q} oninput={onModFilter} placeholder={t('mirror.search')} aria-label={t('mirror.search')} />
          <input class="sm" bind:value={loaderF} oninput={onModFilter} placeholder={t('mirror.loader')} aria-label={t('mirror.loader')} />
          <input class="sm" bind:value={mcF} oninput={onModFilter} placeholder={t('mirror.mc')} aria-label={t('mirror.mc')} />
        </div>
        {#if modsLoading}<div class="muted s">{t('common.loading')}</div>{/if}
        <div class="hits scroll">
          {#each mods as m, i (m.mod_id)}
            <button class="hit row-in" use:stagger={i} animate:settle onclick={() => openMod(m)}>
              <div class="info">
                <div class="t">
                  {m.name}
                  {#if m.author}<span class="faint">· {t('mirror.by', { author: m.author })}</span>{/if}
                </div>
                <div class="d muted mono">
                  {#if m.loaders.length}{m.loaders.join(', ')}{/if}
                  {#if m.mc_versions.length} · {m.mc_versions.join(', ')}{/if}
                </div>
              </div>
              <span class="cnt faint mono">{t('mirror.versionsN', { n: m.version_count })}</span>
            </button>
          {/each}
          {#if mods.length === 0 && !modsLoading}<div class="muted s">{t('mirror.noMods')}</div>{/if}
        </div>
      {:else}
        <div class="ph row">
          <button onclick={() => (selMod = null)}>{t('mrp.back')}</button>
          <div class="seltitle">{selMod.name}</div>
        </div>
        {#if versLoading}<div class="muted s">{t('common.loading')}</div>{/if}
        <div class="hits scroll">
          {#each modVersions as v (v.sha1)}
            <button class="vrow" disabled={!sourceFor(v) || inPack(v)} onclick={() => pickVersion(v)}>
              <div class="info">
                <div class="t">
                  <span class="mono">{v.version}</span>
                  {#if srcTag(v)}<span class="chip">{srcTag(v)}</span>{:else}<span class="chip warn">{t('mirror.unavailable')}</span>{/if}
                  {#if inPack(v)}<span class="chip">{t('mirror.inPack')}</span>{/if}
                </div>
                <div class="d muted mono">
                  {v.targets.join(', ')}{#if v.mc_versions.length} · {v.mc_versions.join(', ')}{/if} · {fmtBytes(v.size_bytes)}
                </div>
              </div>
              <span class="cnt faint mono">{v.sha1.slice(0, 10)}</span>
            </button>
          {/each}
          {#if modVersions.length === 0 && !versLoading}<div class="muted s">{t('mirror.noVersions')}</div>{/if}
        </div>
      {/if}
    {:else if mode === 'builds'}
      {#if !selBuild}
        {#if buildsLoading}<div class="muted s">{t('common.loading')}</div>{/if}
        <div class="hits scroll">
          {#each builds as b (b.pack_id + b.pack_version)}
            <button class="hit" onclick={() => openBuild(b)}>
              <div class="info">
                <div class="t">
                  {b.pack_id} <span class="faint mono">{b.pack_version}</span>
                  {#if b.is_latest}<span class="chip ok">{t('mirror.latest')}</span>{/if}
                </div>
                <div class="d muted mono">
                  {b.mc_version}{#if b.loader_id} · {b.loader_id}{#if b.loader_version} {b.loader_version}{/if}{/if}
                </div>
              </div>
              <span class="cnt faint mono">{t('mirror.modsN', { n: b.mod_count })}</span>
            </button>
          {/each}
          {#if builds.length === 0 && !buildsLoading}<div class="muted s">{t('mirror.noBuilds')}</div>{/if}
        </div>
      {:else}
        <div class="ph row">
          <button onclick={() => (selBuild = null)}>{t('mrp.back')}</button>
          <div class="seltitle">{selBuild.pack_id} <span class="faint mono">{selBuild.pack_version}</span></div>
          {#if allowMany && onAddMany && buildModRows.length}
            <button class="primary sm" onclick={addAllFromBuild}>{t('mirror.addAll', { n: buildModRows.length })}</button>
          {/if}
        </div>
        {#if bmLoading}<div class="muted s">{t('common.loading')}</div>{/if}
        <div class="hits scroll">
          {#each buildModRows as m (m.sha1 + m.filename)}
            <div class="vrow static">
              <div class="info">
                <div class="t">
                  {m.name} <span class="faint mono">{m.version}</span>
                  {#if !m.required}<span class="chip">{t('mr.optional')}</span>{/if}
                  {#if srcTag(m)}<span class="chip">{srcTag(m)}</span>{:else}<span class="chip warn">{t('mirror.unavailable')}</span>{/if}
                  {#if inPack(m)}<span class="chip">{t('mirror.inPack')}</span>{/if}
                </div>
                <div class="d muted mono">
                  {m.filename}{#if m.targets.length} · {m.targets.join(', ')}{/if}
                </div>
              </div>
              <button class="sm" disabled={addedShas.has(m.sha1) || !sourceFor(m) || inPack(m)} onclick={() => addBuildMod(m)}>
                {addedShas.has(m.sha1) ? t('mirror.added') : t('mirror.add')}
              </button>
            </div>
          {/each}
          {#if buildModRows.length === 0 && !bmLoading}<div class="muted s">{t('mirror.noBuildMods')}</div>{/if}
        </div>
        {#if allowMany && onAddAsset && buildAssetRows.length}
          <div class="ph row asec">
            <span class="muted s">{t('mirror.assets')}</span>
            <div class="spacer"></div>
            <button class="primary sm" onclick={addAllAssets}>{t('mirror.addAllAssets', { n: buildAssetRows.length })}</button>
          </div>
          <div class="hits scroll">
            {#each buildAssetRows as a (a.dest)}
              <div class="vrow static">
                <div class="info">
                  <div class="t">
                    {a.dest}
                    {#if !a.required}<span class="chip">{t('mr.optional')}</span>{/if}
                  </div>
                  <div class="d muted mono">{a.source.type}</div>
                </div>
                <button class="sm" disabled={addedDests.has(a.dest)} onclick={() => addBuildAsset(a)}>
                  {addedDests.has(a.dest) ? t('mirror.added') : t('mirror.add')}
                </button>
              </div>
            {/each}
          </div>
        {/if}
      {/if}
    {:else}
      <div class="filters">
        <input class="grow" bind:value={rawQ} placeholder={t('cachePick.search')} aria-label={t('cachePick.search')} />
      </div>
      {#if rawLoading}<div class="muted s">{t('common.loading')}</div>{/if}
      <div class="hits scroll">
        {#each rawShown as e (e.sha1)}
          <button
            class="hit"
            disabled={presentSet.has(`c:${e.sha1}`)}
            onclick={() => onPick({ filename: rawName(e) || `${e.sha1.slice(0, 12)}.jar`, source: { type: 'smrt_cache', sha1: e.sha1 } })}
          >
            <div class="info">
              <div class="t">
                {rawName(e) || t('cachePick.noName')}
                {#if presentSet.has(`c:${e.sha1}`)}<span class="chip">{t('mirror.inPack')}</span>{/if}
                {#if e.uses.length === 0}<span class="chip warn">{t('cachePick.orphan')}</span>{/if}
              </div>
              <div class="d muted mono">{e.sha1.slice(0, 16)} · {fmtBytes(e.size_bytes)}</div>
            </div>
          </button>
        {/each}
        {#if rawShown.length === 0 && !rawLoading}
          <div class="muted s">{rawQ.trim() ? t('cachePick.noMatch') : t('cachePick.empty')}</div>
        {/if}
      </div>
    {/if}
  </Dialog.Content>
</Dialog.Root>

<style>
  /* Panel rides on a Bits component -> global, uniquely named to dodge the
     DialogHost .dlg/.overlay globals. Backdrop is the shared .dlg-scrim. */
  :global(.mirror-dlg) {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    z-index: 61;
    width: 660px;
    max-width: 92vw;
    max-height: 82vh;
    display: flex;
    flex-direction: column;
    padding: var(--space-4);
  }
  .ph {
    gap: var(--space-2);
    margin-bottom: var(--space-2);
    align-items: center;
  }
  .spacer {
    flex: 1;
  }
  .asec {
    margin-top: var(--space-3);
    align-items: center;
  }
  .seltitle {
    flex: 1;
    min-width: 0;
    font-size: var(--fs-md);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .filters {
    display: flex;
    gap: var(--space-2);
    margin-bottom: var(--space-2);
  }
  .filters .grow {
    flex: 1;
  }
  .filters .sm {
    width: 110px;
  }
  .err {
    color: var(--danger);
    font-size: var(--fs-sm);
    margin-bottom: var(--space-2);
  }
  .s {
    font-size: var(--fs-sm);
    padding: var(--space-2) 0;
  }
  .hits {
    overflow: auto;
  }
  .hit,
  .vrow {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    width: 100%;
    text-align: left;
    padding: var(--space-2);
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    border-bottom: 1px solid var(--seam);
    background: transparent;
  }
  .hit:hover,
  .vrow:not(.static):hover {
    background: var(--panel-2);
  }
  .hit:disabled,
  .vrow:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .hit:disabled:hover,
  .vrow:disabled:hover {
    background: transparent;
  }
  .info {
    flex: 1;
    min-width: 0;
  }
  .t {
    font-size: var(--fs-md);
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .d {
    font-size: var(--fs-xs);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    margin-top: 2px;
  }
  .cnt {
    font-size: var(--fs-xs);
    flex-shrink: 0;
  }
  .chip {
    font-size: var(--fs-xs);
    padding: 1px 6px;
    border: 1px solid var(--seam);
    border-radius: 999px;
    color: var(--fg-dim);
  }
  .chip.ok {
    color: var(--ok);
    border-color: color-mix(in srgb, var(--ok) 45%, transparent);
  }
  .chip.warn {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 45%, transparent);
  }
  button.sm {
    padding: 4px 10px;
    font-size: var(--fs-sm);
    flex-shrink: 0;
  }
  @media (max-width: 560px) {
    :global(.mirror-dlg) {
      padding: var(--space-3);
    }
    .ph {
      flex-wrap: wrap;
      row-gap: var(--space-2);
    }
    .filters {
      flex-wrap: wrap;
    }
    .filters .grow {
      flex: 1 1 100%;
    }
    .filters .sm {
      width: auto;
      flex: 1 1 calc(50% - var(--space-2));
      min-width: 0;
    }
  }
</style>
