<script lang="ts">
  import { api, ApiError } from '../lib/api';
  import { settle, stagger, unroll } from '../lib/motion.svelte';
  import { detailOf, notifyFail, toasts } from '../lib/toasts.svelte';
  import { dialogs } from '../lib/dialogs.svelte';
  import { href, plainClick, route } from '../lib/route.svelte';
  import { t } from '../lib/i18n.svelte';
  import { mirror } from '../lib/mirror.svelte';
  import { isDebug, isOperator } from '../lib/roles';
  import type {
    JarDiff,
    ModSummary,
    ReleaseRow,
    SourceDecl,
    UnassignedJar,
    VersionRow,
  } from '../lib/types';
  import ModIcon from './ModIcon.svelte';
  import IdentityDialog, { type IdentityTarget } from './IdentityDialog.svelte';
  import DropZone from './ui/DropZone.svelte';

  // A member reads the registry -- search, the faceted list, a mod's releases and
  // files -- but does none of its authoring. `canOperate` (admin and up) gates the
  // whole write surface: upload, the needs-identity bucket, assigning/editing a
  // jar's identity, the takedown list, and every per-item action. `canDebug`
  // gates the narrower surgical subset within it (merging two mods, rewriting a
  // release version), debug-only on the server (#39/#13). Both come from one
  // identity read; the list load waits on
  // it so a member never fires an operator-only fetch that would 403.
  let canDebug = $state(false);
  let canOperate = $state(false);

  let mods = $state<ModSummary[]>([]);
  let unassigned = $state<UnassignedJar[]>([]);
  let removed = $state<string[]>([]);
  let loading = $state(true);
  let q = $state('');
  // The index only grows, and every row carries an icon and its facets, so the
  // browser reads it a page at a time rather than laying the whole mirror out on
  // each open. The two facet inputs narrow the same query server-side, the way
  // the mirror picker already filters.
  const PAGE = 60;
  let loaderF = $state('');
  let mcF = $state('');
  // the address of the next page, or null once the index runs out
  let more = $state<string | null>(null);

  // the expanded mod and its lazily-loaded releases
  // More than one mod may be open: comparing two mods' builds is the reason to
  // open them at all, and a single slot made that a matter of remembering what
  // the other one said.
  let openIds = $state<number[]>([]);
  let relsByMod = $state<Record<number, ReleaseRow[]>>({});
  let loadingIds = $state<number[]>([]);
  const isOpen = (id: number) => openIds.includes(id);
  // A file whose sha1 Modrinth confirmed is authentic; a self-hosted file under a
  // mod that ALSO has a Modrinth-verified one is a likely repackage (the SC case).
  // Asked per mod, since several are open at once and the answer is about one.
  const hasVerified = (modId: number) =>
    (relsByMod[modId] ?? []).some((r) => r.files.some((f) => f.modrinth_version_id));

  let idTarget = $state<IdentityTarget | null>(null);

  let uploading = $state(false);
  let upMsg = $state('');


  // Which listing is the current one. Typing narrows the query while a walk
  // into the index may be in flight, and the two write the same list: without
  // this the older read's page could be appended to -- or land on top of -- the
  // newer query's results, leaving rows on screen that do not match what was
  // typed. The commit page guards its two readings the same way.
  let generation = 0;

  async function load() {
    const mine = ++generation;
    loading = true;
    try {
      const page = await api.registryMods(
        q.trim() || undefined,
        loaderF.trim() || undefined,
        mcF.trim() || undefined,
        PAGE,
      );
      if (mine !== generation) return;
      mods = page.rows;
      more = page.next;
      // the needs-identity bucket and the takedown list are operator-only reads
      if (canOperate) {
        const [u, rm] = await Promise.all([api.unassigned(), api.removed()]);
        if (mine !== generation) return;
        unassigned = u;
        removed = rm.removed;
      }
    } catch (e) {
      if (mine !== generation) return;
      notifyFail(e);
    } finally {
      if (mine === generation) loading = false;
    }
  }

  async function init() {
    try {
      const m = await api.me();
      canDebug = isDebug(m?.role);
      canOperate = isOperator(m?.role);
    } catch {
      // an identity read failure just leaves the read-only view; load() surfaces
      // any real error
    }
    await load();
  }
  init();

  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  // any change to the query or the facets is a different listing, so it starts
  // the walk over rather than continuing the old one
  function onSearch() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(load, 250);
  }

  // Walk further into the index. Paging is keyset, so rows arriving while the
  // walk is open land outside the page being read rather than shifting it --
  // which is what makes appending safe. A registry event still reloads from the
  // top, because a rename or a merge changes what the pages already read said.
  async function loadMore() {
    if (!more || loading) return;
    const mine = ++generation;
    loading = true;
    try {
      const page = await api.registryModsPage(more);
      // a narrower query started while this page was in flight: its rows are
      // the answer now, and appending to them would mix two listings
      if (mine !== generation) return;
      mods = [...mods, ...page.rows];
      more = page.next;
    } catch (e) {
      if (mine !== generation) return;
      notifyFail(e);
    } finally {
      if (mine === generation) loading = false;
    }
  }

  // The list avatar's icon source: prefer the Modrinth project icon, else the
  // newest cached jar's embedded icon; ModIcon falls back to a letter when a mod
  // has neither (or the icon 404s).
  function iconSource(m: ModSummary): SourceDecl {
    if (m.modrinth_project_id)
      return { type: 'modrinth', project_id: m.modrinth_project_id, version_id: '' };
    if (m.icon_sha1) return { type: 'smrt_cache', sha1: m.icon_sha1 };
    return { type: 'smrt_static', rel_path: '' };
  }

  async function toggle(m: ModSummary) {
    if (isOpen(m.mod_id)) {
      openIds = openIds.filter((id) => id !== m.mod_id);
      const { [m.mod_id]: _dropped, ...rest } = relsByMod;
      relsByMod = rest;
      return;
    }
    openIds = [...openIds, m.mod_id];
    await loadReleases(m.mod_id);
  }

  async function loadReleases(modId: number) {
    loadingIds = [...loadingIds, modId];
    try {
      relsByMod = { ...relsByMod, [modId]: await api.modReleases(modId) };
    } catch (e) {
      notifyFail(e);
    } finally {
      loadingIds = loadingIds.filter((id) => id !== modId);
    }
  }

  // after an edit lands, every open mod is refreshed: the change may have moved
  // a file between two of them (a merge, a re-identified jar)
  async function reloadOpen() {
    await Promise.all(openIds.map(loadReleases));
  }

  async function onDropJars(files: File[]) {
    uploading = true;
    upMsg = '';
    let n = 0;
    try {
      for (const f of files) {
        if (!f.name.toLowerCase().endsWith('.jar')) continue;
        await api.uploadCacheJar(f);
        n++;
      }
      upMsg = t('mm.uploaded', { count: n });
    } catch (x) {
      upMsg = detailOf(x);
    } finally {
      await load();
      uploading = false;
    }
  }

  function assign(u: UnassignedJar) {
    idTarget = { sha1: u.sha1, filename: null, mode: 'assign' };
  }

  function editFile(f: VersionRow, rel: ReleaseRow, modName: string, modId: number) {
    idTarget = {
      sha1: f.sha1,
      filename: f.filename ?? null,
      mode: 'edit',
      modId,
      modName,
      version_number: rel.version_number,
      channel: rel.channel,
      loaders: f.targets.filter((x) => x !== 'any'),
      mc_versions: f.mc_versions,
    };
  }

  async function onSaved() {
    idTarget = null;
    await load();
    await reloadOpen();
  }

  async function rename(m: ModSummary, e: Event) {
    e.stopPropagation();
    const name = (
      await dialogs.prompt(t('mm.renamePrompt'), { title: t('mm.renameTitle'), initial: m.name })
    )?.trim();
    if (!name) return;
    try {
      await api.renameMod(m.mod_id, { name });
      await load();
    } catch (x) {
      notifyFail(x);
    }
  }

  // Merge this mod into another (the target survives). Debug-only registry
  // surgery: the operator gives the surviving mod's id (shown as #id on each row).
  async function merge(m: ModSummary, e: Event) {
    e.stopPropagation();
    const raw = await dialogs.prompt(t('mm.mergePrompt', { name: m.name, id: m.mod_id }), {
      title: t('mm.mergeTitle'),
    });
    if (raw == null) return;
    const into = parseInt(raw.trim(), 10);
    if (!Number.isFinite(into) || into === m.mod_id) {
      toasts.push({ kind: 'error', text: t('mm.mergeBadId') });
      return;
    }
    const ok = await dialogs.confirm(t('mm.mergeConfirm', { from: m.mod_id, into }), {
      danger: true,
    });
    if (!ok) return;
    try {
      await api.mergeMods(m.mod_id, into);
      openIds = openIds.filter((id) => id !== m.mod_id);
      await load();
    } catch (x) {
      notifyFail(x);
    }
  }

  // Repackage (tamper) diff: for a self-hosted file under a mod that also has a
  // Modrinth-verified sibling, show what it changed vs the genuine build. Toggles
  // an inline panel; the changed classes are the signal, resources are noise.
  let diffFor = $state<string | null>(null);
  let diffData = $state<JarDiff | null>(null);
  let diffLoading = $state(false);
  let diffErr = $state('');

  async function showDiff(f: VersionRow) {
    if (diffFor === f.sha1) {
      diffFor = null;
      diffData = null;
      return;
    }
    diffFor = f.sha1;
    diffData = null;
    diffErr = '';
    diffLoading = true;
    try {
      diffData = await api.repackDiff(f.sha1);
    } catch (e) {
      diffErr = detailOf(e);
    } finally {
      diffLoading = false;
    }
  }

  async function editReleaseVersion(rel: ReleaseRow) {
    const v = (
      await dialogs.prompt(t('mm.versionPrompt'), {
        title: t('mm.editReleaseTitle'),
        initial: rel.version_number,
      })
    )?.trim();
    if (!v || v === rel.version_number) return;
    try {
      await api.editRelease(rel.release_id, { version_number: v });
      await reloadOpen();
    } catch (x) {
      notifyFail(x);
    }
  }

  async function delFile(f: VersionRow) {
    const name = f.filename || f.sha1.slice(0, 12);
    const ok = await dialogs.confirm(t('cache.deleteMsg', { name }), {
      title: t('cache.deleteTitle'),
      danger: true,
    });
    if (!ok) return;
    try {
      await api.deleteCacheJar(f.sha1);
      await load();
      await reloadOpen();
    } catch (x) {
      notifyFail(x);
    }
  }

  // A deliberate policy block, not a cleanup delete: drops the bytes and
  // tombstones the sha1 so it cannot be re-served or re-added (#14).
  async function takedown(f: VersionRow) {
    const name = f.filename || f.sha1.slice(0, 12);
    const ok = await dialogs.confirm(t('cache.takedownMsg', { name }), {
      title: t('cache.takedownTitle'),
      danger: true,
    });
    if (!ok) return;
    try {
      await api.takedownJar(f.sha1);
      await load();
      await reloadOpen();
    } catch (x) {
      notifyFail(x);
    }
  }

  async function restore(sha1: string) {
    try {
      await api.restoreJar(sha1);
      removed = removed.filter((s) => s !== sha1);
    } catch (x) {
      notifyFail(x);
    }
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

  // Wide mods span many MC versions; the backend returns them oldest-first, so a
  // long run collapses to its bounds plus a count rather than a flat tag soup.
  function mcFacet(vs: string[]): { span: boolean; items: string[]; count: number } {
    if (vs.length <= 4) return { span: false, items: vs, count: vs.length };
    return { span: true, items: [vs[0], vs[vs.length - 1]], count: vs.length };
  }

  // every registry write, and every harvest, changes what this lists
  $effect(() => {
    if (mirror.registry > 0) load();
  });
</script>

<div class="view">

  {#if canOperate}
    <DropZone
      accept=".jar"
      label={uploading ? t('mm.uploading') : t('mm.drop')}
      busy={uploading}
      onFiles={onDropJars}
    />
    {#if upMsg}<div class="upmsg muted mono">{upMsg}</div>{/if}
  {/if}

  {#if canOperate && unassigned.length}
    <section class="panel bucket">
      <div class="bhead">
        <span class="btitle">{t('mm.needsIdentity')}</span>
        <span class="faint">{t('mm.needsIdentitySub', { n: unassigned.length })}</span>
      </div>
      {#each unassigned as u (u.sha1)}
        <!-- What the harvest read out of the jar (#123). It was a hash and a
             file size, so naming one meant downloading and opening it by hand,
             which is why the bucket only grew. A jar that declares nothing
             still shows as it always did -- that is the honest answer for it. -->
        <div class="urow">
          <div class="uinfo">
            {#if u.name || u.modid}
              <span class="uname">{u.name ?? u.modid}</span>
              {#if u.version}<span class="chip">{u.version}</span>{/if}
              {#if u.kind && u.kind !== 'mod'}<span class="chip kind">{u.kind}</span>{/if}
              <span class="faint">{[u.loaders, u.mc].filter(Boolean).join(' · ')}</span>
            {:else if u.filename}
              <span class="uname mono">{u.filename}</span>
              <span class="faint">{t('mm.declaresNothing')}</span>
            {:else}
              <span class="mono">{u.sha1.slice(0, 16)}</span>
              <span class="faint">{t('mm.unread')}</span>
            {/if}
            <span class="faint mono">{fmtBytes(u.size_bytes)}</span>
          </div>
          <button class="primary sm" onclick={() => assign(u)}>{t('mm.assign')}</button>
        </div>
      {/each}
    </section>
  {/if}

  <div class="filters">
    <input class="grow" bind:value={q} oninput={onSearch} placeholder={t('mm.search')} aria-label={t('mm.search')} />
    <input class="sm" bind:value={loaderF} oninput={onSearch} placeholder={t('mirror.loader')} aria-label={t('mirror.loader')} />
    <input class="sm" bind:value={mcF} oninput={onSearch} placeholder={t('mirror.mc')} aria-label={t('mirror.mc')} />
  </div>

  <div class="panel modlist">
    {#each mods as m, i (m.mod_id)}
      <div class="mod row-in" class:open={isOpen(m.mod_id)} use:stagger={i} animate:settle>
        <div
          class="modrow"
          role="button"
          tabindex="0"
          aria-expanded={isOpen(m.mod_id)}
          onclick={() => toggle(m)}
          onkeydown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault();
              toggle(m);
            }
          }}
        >
          <span class="chev" aria-hidden="true">&#9656;</span>
          <ModIcon name={m.name} source={iconSource(m)} sha1={m.icon_sha1 ?? null} size={32} mono />
          <div class="minfo">
            <div class="mname">
              <a
                class="namelink"
                href={href.mod(m.mod_id)}
                onclick={(e) => {
                  e.stopPropagation();
                  if (!plainClick(e)) return;
                  e.preventDefault();
                  route.openMod(m.mod_id);
                }}>{m.name}</a>{#if m.author}<span class="mby">{t('mm.by', { author: m.author })}</span>{/if}
            </div>
            {#if m.loaders.length || m.mc_versions.length}
              <div class="mtags">
                {#if m.loaders.length}
                  <span class="facet">
                    {#each m.loaders as l}<span class="tag">{l}</span>{/each}
                  </span>
                {/if}
                {#if m.mc_versions.length}
                  {@const mc = mcFacet(m.mc_versions)}
                  <span class="facet mc">
                    {#if mc.span}
                      <span class="tag">{mc.items[0]}</span>
                      <span class="ell" aria-hidden="true">&hellip;</span>
                      <span class="tag">{mc.items[1]}</span>
                      <span class="fcount mono">{mc.count}</span>
                    {:else}
                      {#each mc.items as v}<span class="tag">{v}</span>{/each}
                    {/if}
                  </span>
                {/if}
              </div>
            {/if}
          </div>
          {#if canOperate && isOpen(m.mod_id)}
            <span class="modactions">
              <button class="link" onclick={(e) => rename(m, e)} title={t('mm.renameTitle')}>
                {t('mm.rename')}
              </button>
              {#if canDebug}
                <button class="link" onclick={(e) => merge(m, e)} title={t('mm.mergeTitle')}>
                  {t('mm.merge')}
                </button>
                <span class="faint mono modid">#{m.mod_id}</span>
              {/if}
            </span>
          {/if}
          <span class="cnt mono">{t('mirror.versionsN', { n: m.version_count })}</span>
        </div>

        {#if isOpen(m.mod_id)}
          <!-- the builds unroll under their mod and roll back up: the rows
               below are pushed rather than covered, so the list stays one
               surface instead of becoming a stack of layers -->
          <div class="rels" transition:unroll>
            {#if loadingIds.includes(m.mod_id)}
              <div class="muted s">{t('common.loading')}</div>
            {/if}
            {#each relsByMod[m.mod_id] ?? [] as rel (rel.release_id)}
              <div class="rel">
                <div class="relhead">
                  <span class="rver mono">{rel.version_number}</span>
                  <span class="chip ch-{rel.channel}">{rel.channel}</span>
                  <span class="faint mono">{t('mm.filesN', { n: rel.files.length })}</span>
                  {#if canDebug}
                    <button class="link sm" onclick={() => editReleaseVersion(rel)}>{t('mm.edit')}</button>
                  {/if}
                </div>
                {#each rel.files as f (f.sha1)}
                  <div class="file">
                    <!-- the file's own embedded icon when the mirror holds the
                         jar; otherwise the mod's, because an uncached build has
                         no icon of its own and a letter says less than the mod's
                         face does -->
                    <ModIcon
                      name={f.filename ?? m.name}
                      source={f.cached ? { type: 'smrt_cache', sha1: f.sha1 } : iconSource(m)}
                      sha1={f.cached ? f.sha1 : (m.icon_sha1 ?? null)}
                      size={22}
                      mono
                    />
                    <div class="finfo">
                      <div class="fname">{f.filename ?? f.sha1.slice(0, 16)}</div>
                      <div class="fmeta muted mono">
                        {f.targets.join(', ')}{#if f.mc_versions.length} · {f.mc_versions.join(', ')}{/if}
                        · {fmtBytes(f.size_bytes)}{#if !f.cached} · {t('mm.uncached')}{/if}
                      </div>
                    </div>
                    {#if f.modrinth_version_id}
                      <span class="chip verified" title="Modrinth-verified">{t('mm.verified')}</span>
                    {:else if hasVerified(m.mod_id)}
                      <span class="chip repack" title={t('mm.repackHint')}>{t('mm.repack')}</span>
                    {:else}
                      <span class="chip">{t('mm.selfhost')}</span>
                    {/if}
                    {#if canOperate}
                      <div class="factions">
                        {#if !f.modrinth_version_id && hasVerified(m.mod_id) && f.cached}
                          <button
                            class="link"
                            class:active={diffFor === f.sha1}
                            onclick={() => showDiff(f)}>{t('mm.diff')}</button>
                        {/if}
                        <button class="link" onclick={() => editFile(f, rel, m.name, m.mod_id)}>{t('mm.edit')}</button>
                        <button class="link" onclick={() => delFile(f)}>{t('common.delete')}</button>
                        <button class="link danger" onclick={() => takedown(f)} title={t('mm.takedownHint')}>{t('mm.takedown')}</button>
                      </div>
                    {/if}
                  </div>
                  {#if diffFor === f.sha1}
                    <div class="diffpanel">
                      {#if diffLoading}
                        <div class="muted s">{t('common.loading')}</div>
                      {:else if diffErr}
                        <div class="err mono">{diffErr}</div>
                      {:else if diffData}
                        <div class="diffsum mono">
                          {t('mm.diffClasses', { n: diffData.changed_classes.length })} ·
                          {t('mm.diffResources', { n: diffData.changed_resources.length })} ·
                          {t('mm.diffAdded', { n: diffData.added.length })} ·
                          {t('mm.diffRemoved', { n: diffData.removed.length })} ·
                          {t('mm.diffIdentical', { n: diffData.identical })}
                        </div>
                        {#if diffData.changed_classes.length}
                          <div class="diffh">{t('mm.diffClassesH')}</div>
                          {#each diffData.changed_classes as c}
                            <div class="mono diffrow">{c}</div>
                          {/each}
                        {:else}
                          <div class="muted s">{t('mm.diffNoClasses')}</div>
                        {/if}
                      {/if}
                    </div>
                  {/if}
                {/each}
              </div>
            {/each}
            {#if !loadingIds.includes(m.mod_id) && (relsByMod[m.mod_id] ?? []).length === 0}
              <div class="muted s">{t('mirror.noVersions')}</div>
            {/if}
          </div>
        {/if}
      </div>
    {/each}
    {#if mods.length === 0 && !loading}
      <div class="muted empty">{t('mm.noMods')}</div>
    {/if}
  </div>
  {#if more}
    <button class="sm more" onclick={loadMore} disabled={loading}>{t('mm.more')}</button>
  {/if}

  {#if canOperate && removed.length}
    <h2 class="sec">{t('cache.removedTitle')}</h2>
    <div class="cache-head muted">{t('cache.removedSub', { count: removed.length })}</div>
    <div class="panel">
      {#each removed as sha}
        <div class="rmrow">
          <span class="mono faint">{sha}</span>
          <button class="link" onclick={() => restore(sha)} title={t('cache.restoreHint')}>
            {t('cache.restore')}
          </button>
        </div>
      {/each}
    </div>
  {/if}

  {#if idTarget}
    {#key idTarget.sha1}
      <IdentityDialog target={idTarget} {mods} {onSaved} onClose={() => (idTarget = null)} />
    {/key}
  {/if}
</div>

<style>
  .view {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .err {
    color: var(--danger);
    background: var(--danger-soft);
    border: 1px solid color-mix(in srgb, var(--danger) 40%, transparent);
    border-radius: var(--radius-sm);
    padding: var(--space-3) var(--space-4);
    font-size: var(--fs-sm);
  }
  .upmsg {
    font-size: var(--fs-sm);
    margin-top: -8px;
  }
  .bucket {
    padding: var(--space-3);
    display: flex;
    flex-direction: column;
    gap: 4px;
    border-color: color-mix(in srgb, var(--warn) 35%, var(--seam));
  }
  .bhead {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    margin-bottom: 4px;
  }
  .btitle {
    font-size: var(--fs-md);
    color: var(--warn);
  }
  .uname {
    font-weight: 600;
  }
  .chip.kind {
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .urow {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: 5px 4px;
    border-top: 1px solid var(--seam);
  }
  .uinfo {
    flex: 1;
    display: flex;
    gap: var(--space-3);
    font-size: var(--fs-sm);
  }
  .filters {
    display: flex;
    gap: var(--space-2);
    max-width: 640px;
  }
  .filters .grow {
    flex: 1;
  }
  .filters .sm {
    width: 110px;
  }
  .more {
    align-self: center;
  }
  .modlist {
    overflow: hidden;
  }
  .mod {
    border-bottom: 1px solid var(--seam);
  }
  .mod:last-child {
    border-bottom: none;
  }
  .modrow {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    width: 100%;
    text-align: left;
    background: transparent;
    border: none;
    border-radius: 0;
    padding: var(--space-3);
    cursor: pointer;
  }
  .modrow:hover {
    background: var(--panel-2);
  }
  .chev {
    color: var(--fg-faint);
    font-size: var(--fs-xs);
    flex: none;
    transition: transform var(--dur-state) var(--ease-out);
  }
  .mod.open .chev {
    transform: rotate(90deg);
    color: var(--fg-dim);
  }
  .minfo {
    flex: 1;
    min-width: 0;
  }
  .mname {
    font-size: var(--fs-lg);
    font-weight: 600;
  }
  .namelink {
    color: inherit;
    text-decoration: none;
    background: transparent;
    border: none;
    border-radius: 0;
    padding: 0;
    font-size: var(--fs-lg);
    font-weight: 600;
    color: var(--fg);
    cursor: pointer;
    text-decoration: underline;
    text-decoration-color: transparent;
    text-underline-offset: 2px;
  }
  .namelink:hover {
    text-decoration-color: var(--seam-bright);
  }
  .mby {
    color: var(--fg-faint);
    font-size: var(--fs-sm);
    font-weight: 400;
    margin-left: 6px;
  }
  .mtags {
    display: flex;
    gap: 5px;
    margin-top: 6px;
    flex-wrap: wrap;
    align-items: center;
  }
  .facet {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    flex-wrap: wrap;
  }
  .facet.mc {
    padding-left: 8px;
    margin-left: 3px;
    border-left: 1px solid var(--seam);
  }
  .ell {
    color: var(--fg-faint);
    font-size: var(--fs-xs);
  }
  .fcount {
    font-size: var(--fs-xs);
    color: var(--fg-faint);
    margin-left: 1px;
  }
  .modactions {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    flex: none;
  }
  .modid {
    font-size: var(--fs-xs);
  }
  .factions {
    display: flex;
    gap: 2px;
    flex-shrink: 0;
  }
  .link.active {
    color: var(--accent-strong);
  }
  .diffpanel {
    margin: 2px 0 var(--space-3) 34px;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--seam);
    border-radius: var(--radius-sm);
    background: var(--panel-2);
  }
  .diffsum {
    font-size: var(--fs-xs);
    color: var(--fg-dim);
    margin-bottom: 6px;
  }
  .diffh {
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--warn);
    margin: 4px 0;
  }
  .diffrow {
    font-size: var(--fs-xs);
    padding: 1px 0;
    overflow-wrap: anywhere;
  }
  .cnt {
    font-size: var(--fs-xs);
    color: var(--fg-faint);
    flex-shrink: 0;
  }
  .rels {
    padding: 2px 0 var(--space-3) 42px;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }
  .rel {
    border-left: 2px solid var(--seam-bright);
    padding-left: var(--space-3);
  }
  .relhead {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 4px 0;
  }
  .rver {
    font-size: var(--fs-sm);
  }
  .file {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 3px 0;
  }
  .finfo {
    flex: 1;
    min-width: 0;
  }
  .fname {
    font-size: var(--fs-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .fmeta {
    font-size: var(--fs-xs);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip {
    font-size: var(--fs-xs);
    padding: 1px 7px;
    border: 1px solid var(--seam);
    border-radius: 999px;
    color: var(--fg-dim);
    flex-shrink: 0;
  }
  .chip.verified {
    color: var(--info);
    border-color: color-mix(in srgb, var(--info) 45%, var(--seam));
    background: var(--info-soft);
  }
  .chip.repack {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 45%, var(--seam));
    background: var(--warn-soft);
  }
  .link {
    background: transparent;
    border: none;
    border-radius: 0;
    color: var(--fg-dim);
    padding: 2px 6px;
    font-size: var(--fs-xs);
    flex-shrink: 0;
  }
  .link:hover {
    color: var(--fg);
  }
  .link.danger:hover {
    color: var(--danger);
  }
  button.sm {
    padding: 4px 10px;
    font-size: var(--fs-sm);
    flex-shrink: 0;
  }
  .empty,
  .s {
    padding: var(--space-3);
    font-size: var(--fs-sm);
  }
  .sec {
    font-size: var(--fs-md);
    color: var(--fg-dim);
    text-transform: uppercase;
    letter-spacing: 0.08em;
    margin: var(--space-3) 0 6px;
  }
  .cache-head {
    font-size: var(--fs-sm);
    margin-bottom: 8px;
  }
  .rmrow {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    padding: 4px var(--space-3);
    font-size: var(--fs-xs);
    border-bottom: 1px solid var(--seam);
  }
</style>
