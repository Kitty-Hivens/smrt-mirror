<script lang="ts">
  import { Dialog } from 'bits-ui';
  import { api, ApiError } from '../lib/api';
  import { t } from '../lib/i18n.svelte';
  import type { ModrinthHit, ModrinthVersion } from '../lib/types';
  import Select from './ui/Select.svelte';

  let {
    mc,
    loader,
    projectType = 'mod',
    initialQuery,
    present = [],
    onPick,
    onClose,
  }: {
    mc?: string;
    // current pack loader; pre-selects the version filter so the operator sees
    // only compatible versions first
    loader?: string;
    projectType?: string;
    // pre-filled search (a resolve-report suggestion); searched immediately
    initialQuery?: string;
    // source keys the pack already declares -- those hits are shown, but not
    // offered again: a pack ships one build of a mod, and adding a second row is
    // never what the operator meant. Re-pinning a row excludes that row's own key.
    present?: string[];
    onPick: (sel: { project_id: string; slug: string; version_id: string; title: string }) => void;
    onClose: () => void;
  } = $props();

  const presentSet = $derived(new Set(present));
  const inPack = (projectId: string) => presentSet.has(`m:${projectId}`);
  // Upstream sometimes publishes a version whose jar never landed: the metadata
  // is listed with an empty file array. Such a pin cannot be built, so it is
  // shown (so the operator sees why the newest entry is unavailable) but dead.
  const hasFile = (v: ModrinthVersion) => (v.files?.length ?? 0) > 0;

  // svelte-ignore state_referenced_locally -- a mount-time prefill by design
  let q = $state(initialQuery ?? '');
  let hits = $state<ModrinthHit[]>([]);
  let busy = $state(false);
  let err = $state('');
  let timer: ReturnType<typeof setTimeout> | undefined;

  // step 2: version selection for the picked project
  let sel = $state<ModrinthHit | null>(null);
  let versions = $state<ModrinthVersion[]>([]);
  let loadingVers = $state(false);
  let loaderFilter = $state('');
  let releaseOnly = $state(false);

  function onInput() {
    clearTimeout(timer);
    timer = setTimeout(search, 300);
  }

  // svelte-ignore state_referenced_locally -- fires once for the prefill
  if (q.trim()) {
    void search();
  }

  // Which search is the current one -- see the same guard in ModPicker: the
  // debounce shortens the window but a slow request can still land after a
  // narrower one and answer the wrong query.
  let generation = 0;

  async function search() {
    if (!q.trim()) {
      hits = [];
      return;
    }
    const mine = ++generation;
    busy = true;
    err = '';
    try {
      const found = await api.modrinthSearch(q.trim(), mc, projectType);
      if (mine !== generation) return;
      hits = found;
    } catch (e) {
      if (mine !== generation) return;
      err = e instanceof ApiError ? `${e.status} ${e.body}` : String(e);
    } finally {
      if (mine === generation) busy = false;
    }
  }

  async function openVersions(h: ModrinthHit) {
    sel = h;
    versions = [];
    err = '';
    loadingVers = true;
    // default the loader filter to the pack's loader when the project ships it
    loaderFilter = '';
    try {
      versions = await api.modrinthVersions(h.slug, mc);
      if (loader && versions.some((v) => v.loaders.includes(loader!))) loaderFilter = loader;
    } catch (e) {
      err = e instanceof ApiError ? `${e.status} ${e.body}` : String(e);
    } finally {
      loadingVers = false;
    }
  }

  const shownVersions = $derived(
    versions.filter(
      (v) =>
        (!loaderFilter || v.loaders.includes(loaderFilter)) &&
        (!releaseOnly || v.version_type === 'release'),
    ),
  );

  // loaders actually offered by this project's versions, for the filter chips
  const loaderOptions = $derived([...new Set(versions.flatMap((v) => v.loaders))].sort());
  const loaderSelOptions = $derived([
    { value: '', label: t('mrp.anyLoader') },
    ...loaderOptions.map((l) => ({ value: l, label: l })),
  ]);

  function choose(v: ModrinthVersion) {
    if (!sel || !hasFile(v)) return;
    onPick({ project_id: sel.project_id, slug: sel.slug, version_id: v.id, title: sel.title });
  }

  function back() {
    sel = null;
    versions = [];
    err = '';
  }

  // escape / outside-click flip Bits' open to false; the parent unmounts us on close
  function onOpenChange(open: boolean) {
    if (!open) onClose();
  }
</script>

<Dialog.Root open {onOpenChange}>
  <Dialog.Overlay class="dlg-scrim" />
  <Dialog.Content class="mrp-dlg panel">
    <Dialog.Title class="vh">{t('mrp.title')}</Dialog.Title>
    {#if !sel}
      <div class="ph row">
        <input bind:value={q} oninput={onInput} placeholder={t('mrp.search')} aria-label={t('mrp.search')} />
        <button onclick={onClose}>{t('common.close')}</button>
      </div>
      {#if err}<div class="err mono">{err}</div>{/if}
      {#if busy}<div class="muted s">{t('mrp.searching')}</div>{/if}
      <div class="hits scroll">
        {#each hits as h}
          <button class="hit" disabled={inPack(h.project_id)} onclick={() => openVersions(h)}>
            {#if h.icon_url}<img src={h.icon_url} alt="" />{:else}<div class="ic"></div>{/if}
            <div class="info">
              <div class="t">
                {h.title} <span class="faint mono">{h.slug}</span>
                {#if inPack(h.project_id)}<span class="chip muted">{t('mrp.inPack')}</span>{/if}
                {#if h.author}<span class="faint">· {t('mrp.by', { author: h.author })}</span>{/if}
              </div>
              <div class="d muted">{h.description}</div>
            </div>
          </button>
        {/each}
        {#if hits.length === 0 && q.trim() && !busy}<div class="muted s">{t('mrp.noResults')}</div>{/if}
      </div>
    {:else}
      <div class="ph row">
        <button onclick={back}>{t('mrp.back')}</button>
        <div class="seltitle">
          {sel.title} <span class="faint mono">{sel.slug}</span>
        </div>
        <button onclick={onClose}>{t('common.close')}</button>
      </div>
      <div class="filters">
        <span class="fl">
          {t('mrp.loader')}
          <Select compact bind:value={loaderFilter} options={loaderSelOptions} ariaLabel={t('mrp.loader')} />
        </span>
        <label class="fl chk"><input type="checkbox" bind:checked={releaseOnly} /> {t('mrp.releaseOnly')}</label>
        {#if mc}<span class="faint s">{t('mrp.mcLocked', { mc })}</span>{/if}
      </div>
      {#if err}<div class="err mono">{err}</div>{/if}
      {#if loadingVers}<div class="muted s">{t('mrp.loadingVersions')}</div>{/if}
      <div class="hits scroll">
        {#each shownVersions as v (v.id)}
          <button class="vrow" disabled={!hasFile(v)} onclick={() => choose(v)}>
            <div class="info">
              <div class="t">
                <span class="vn mono">{v.version_number}</span>
                {#if v.version_type && v.version_type !== 'release'}<span class="chip">{v.version_type}</span>{/if}
                {#if !hasFile(v)}<span class="chip">{t('mrp.noFile')}</span>{/if}
              </div>
              <div class="d muted mono">
                {v.loaders.join(', ')}{#if v.game_versions.length} · {v.game_versions.join(', ')}{/if}
              </div>
            </div>
          </button>
        {/each}
        {#if shownVersions.length === 0 && !loadingVers}<div class="muted s">{t('mrp.noVersions')}</div>{/if}
      </div>
    {/if}
  </Dialog.Content>
</Dialog.Root>

<style>
  /* Panel rides on a Bits component -> global, uniquely named to dodge the
     DialogHost .dlg/.overlay globals. Backdrop is the shared .dlg-scrim. */
  :global(.mrp-dlg) {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    z-index: 61;
    width: 620px;
    max-width: 92vw;
    max-height: 80vh;
    display: flex;
    flex-direction: column;
    padding: 16px;
  }
  .ph {
    gap: 10px;
    margin-bottom: 10px;
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
    align-items: center;
    gap: 14px;
    margin-bottom: 10px;
    flex-wrap: wrap;
  }
  .fl {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-sm);
    color: var(--fg-dim);
  }
  .chk {
    cursor: pointer;
  }
  .err {
    color: var(--danger);
    font-size: var(--fs-sm);
    margin-bottom: 8px;
  }
  .s {
    font-size: var(--fs-sm);
    padding: 6px 0;
  }
  .hits {
    overflow: auto;
  }
  .hit,
  .vrow {
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    text-align: left;
    padding: 8px;
    border: 1px solid transparent;
    border-bottom: 1px solid var(--seam);
    background: transparent;
  }
  .hit:hover,
  .vrow:hover {
    background: var(--panel-2);
    border-bottom-color: var(--seam);
  }
  .hit img,
  .hit .ic {
    width: 38px;
    height: 38px;
    object-fit: cover;
    background: var(--bg);
    border: 1px solid var(--seam);
    flex-shrink: 0;
  }
  .info {
    flex: 1;
    min-width: 0;
  }
  .t {
    font-size: var(--fs-md);
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .vn {
    font-size: var(--fs-sm);
  }
  .chip {
    font-size: var(--fs-xs);
    padding: 1px 6px;
    border: 1px solid var(--seam);
    border-radius: 999px;
    color: var(--warn);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .chip.muted {
    color: var(--fg-dim);
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
  .d {
    font-size: var(--fs-xs);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  @media (max-width: 560px) {
    :global(.mrp-dlg) {
      padding: var(--space-3);
    }
  }
</style>
