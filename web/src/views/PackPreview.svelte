<script lang="ts">
  import { api, ApiError } from '../lib/api';
  import { notifyFail, toasts } from '../lib/toasts.svelte';
  import { t } from '../lib/i18n.svelte';
  import JobLog from './JobLog.svelte';
  import ModRow from './ModRow.svelte';
  import ModIcon from './ModIcon.svelte';
  import { renderMarkdown } from '../lib/markdown';
  import { diffIsEmpty, diffManifests } from '../lib/diff';
  import { unroll } from '../lib/motion.svelte';
  import {
    assetName,
    bucketAssets,
    conflictIndex,
    formatBytes,
    isLibrary,
    letterAvatar,
    resolveDeps,
  } from '../lib/preview';
  import type { AssetEntry, DryRun, JobStatus, ModEntry, PackManifest } from '../lib/types';

  let { packId }: { packId: string } = $props();

  let jobId = $state<string | null>(null);
  let running = $state(false);
  let showLog = $state(false);
  let result = $state<DryRun | null>(null);
  let prev = $state<PackManifest | null>(null);
  let prevMissing = $state(false);
  let enabled = $state<Record<string, boolean>>({});

  let libsOpen = $state(false);
  let configsOpen = $state(false);
  let diffOpen = $state(false);
  // the hero <img> has no letter-avatar sibling of its own, so a broken icon_url
  // would show the browser's broken-image glyph; this flips it to the avatar
  let heroBroken = $state(false);

  const manifest = $derived(result?.manifest ?? null);
  const summary = $derived(result?.summary ?? null);
  const heroAvatar = $derived(summary ? letterAvatar(summary.display_name) : null);
  // gallery_urls is always serialized (no skip_serializing_if), so this is just
  // a null-summary guard; ts-rs already types it as always-present.
  const gallery = $derived(summary?.gallery_urls ?? []);
  const mods = $derived<ModEntry[]>(manifest?.mods ?? []);
  const ungroupedNonLib = $derived(mods.filter((m) => !isLibrary(m)));
  const libraries = $derived(mods.filter(isLibrary));
  const buckets = $derived(manifest ? bucketAssets(manifest.assets) : null);
  const dep = $derived(resolveDeps(mods));
  const conflictIdx = $derived(conflictIndex(mods));
  const diff = $derived(prev && manifest ? diffManifests(prev, manifest) : null);

  function initEnabled(list: ModEntry[]) {
    const next: Record<string, boolean> = {};
    for (const m of list) next[m.filename] = m.required || (m.default_enabled ?? true);
    enabled = next;
  }

  function toggle(filename: string, on: boolean) {
    const next = { ...enabled, [filename]: on };
    if (on) for (const other of conflictIdx.get(filename) ?? []) next[other] = false;
    enabled = next;
  }

  function conflictsFor(filename: string): string[] {
    return [...(conflictIdx.get(filename) ?? [])];
  }

  async function runPreview() {
    running = true;
    result = null;
    heroBroken = false;
    showLog = true;
    jobId = null;
    try {
      const { job_id } = await api.buildPack(packId, { dryRun: true });
      jobId = job_id;
    } catch (e) {
      notifyFail(e);
      running = false;
    }
  }

  async function onJobDone(status: JobStatus) {
    running = false;
    if (status !== 'done') return;
    if (!jobId) return;
    try {
      const js = await api.jobStatus(jobId);
      if (js.result) {
        initEnabled(js.result.manifest.mods);
        result = js.result;
        showLog = false;
      } else {
        toasts.push({ kind: 'error', text: t('prev.noResult') });
      }
    } catch (e) {
      notifyFail(e);
    }
  }

  async function loadPrev() {
    try {
      prev = await api.manifest(packId);
    } catch (e) {
      prev = null;
      prevMissing = e instanceof ApiError && e.status === 404;
    }
  }

  runPreview();
  loadPrev();
</script>

<div class="preview">
  <div class="ctl">
    <span class="ttl">{t('prev.title')}</span>
    <span class="sub">{t('prev.subtitle')}</span>
    <span class="sp"></span>
    {#if manifest}<span class="ver mono">{manifest.pack_version}</span>{/if}
    <button class="refresh" onclick={runPreview} disabled={running}>
      {running ? t('bld.building') : t('prev.rebuild')}
    </button>
  </div>


  {#if running && !jobId}
    <div class="note">{t('prev.starting')}</div>
  {/if}
  {#if jobId && (running || showLog)}
    {#key jobId}<JobLog {jobId} onDone={onJobDone} />{/key}
  {/if}

  {#if running && !manifest}
    <!-- a preview needs a full dry-run build, so shape the wait as a loading
         placeholder rather than an empty panel that reads as broken -->
    <div class="skeleton" aria-hidden="true">
      <div class="sk sk-hero"></div>
      <div class="sk sk-row"></div>
      <div class="sk sk-row"></div>
      <div class="sk sk-row"></div>
    </div>
  {/if}

  {#if manifest && summary}
    <!-- What the pre-publish check found. The preview hides the job log once
         the manifest is in, so the verdict rides on the manifest instead of
         scrolling away with it. -->
    {#if manifest.checks}
      <div class="checks" class:bad={(manifest.checks.blocking ?? []).length > 0}>
        {#if (manifest.checks.blocking ?? []).length}
          <strong>{t('prev.wouldNotStart')}</strong>
          <ul>
            {#each manifest.checks.blocking ?? [] as line (line)}<li>{line}</li>{/each}
          </ul>
        {/if}
        {#if (manifest.checks.advisory ?? []).length}
          <span class="faint">{t('prev.noted')}</span>
          <ul class="faint">
            {#each manifest.checks.advisory ?? [] as line (line)}<li>{line}</li>{/each}
          </ul>
        {/if}
      </div>
    {/if}

    <!-- version diff vs published -->
    {#if diff}
      <div class="diff" class:clean={diffIsEmpty(diff)}>
        {#if diffIsEmpty(diff)}
          <span class="ok">{t('prev.noChanges', { v: diff.prevVersion })}</span>
        {:else}
          <strong>{t('prev.vs', { v: diff.prevVersion })}</strong>
          {#if diff.added.length}<span class="add">{t('prev.added', { n: diff.added.length })}</span>{/if}
          {#if diff.removed.length}<span class="rem">{t('prev.removed', { n: diff.removed.length })}</span>{/if}
          {#if diff.changed.length}<span class="chg">{t('prev.updated', { n: diff.changed.length })}</span>{/if}
          <span class="faint">{t('prev.unchanged', { n: diff.unchanged })}</span>
          <button class="link" onclick={() => (diffOpen = !diffOpen)}>
            {diffOpen ? t('prev.hide') : t('prev.details')}
          </button>
        {/if}
      </div>
      {#if diffOpen && !diffIsEmpty(diff)}
        <div class="difflist mono">
          {#each diff.added as m (m.filename)}<div class="add">+ {m.filename}</div>{/each}
          {#each diff.removed as m (m.filename)}<div class="rem">- {m.filename}</div>{/each}
          {#each diff.changed as c (c.filename)}<div class="chg">~ {c.filename}</div>{/each}
        </div>
      {/if}
    {:else if prevMissing}
      <div class="diff"><span class="faint">{t('prev.firstVersion')}</span></div>
    {/if}

    <!-- hero -->
    <div
      class="hero"
      style={summary.banner_url
        ? `background-image:linear-gradient(rgba(8,10,18,.45),rgba(8,10,18,.74)), url('${summary.banner_url}')`
        : ''}
    >
      <div class="heroicon">
        {#if summary.icon_url && !heroBroken}
          <img src={summary.icon_url} alt={summary.display_name} onerror={() => (heroBroken = true)} />
        {:else if heroAvatar}
          <span class="avatar" style="background:{heroAvatar.color}">{heroAvatar.initials}</span>
        {/if}
      </div>
      <div class="herotext">
        <h1>{summary.display_name}</h1>
        {#if summary.tagline}<p class="tag">{summary.tagline}</p>{/if}
        <div class="metachips">
          <span class="mc">{manifest.minecraft.version}</span>
          <span class="mc">{manifest.loader.name} {manifest.loader.version}</span>
          <span class="mc">Java {manifest.java.major}</span>
          <span class="mc">{t('prev.modsChip', { n: mods.length })}</span>
          {#if manifest.assets.length}<span class="mc">{t('prev.assetsChip', { n: manifest.assets.length })}</span>{/if}
        </div>
      </div>
    </div>

    <!-- about -->
    {#if summary.description_md}
      <div class="card about">
        <!-- renderMarkdown emits a sanitised CommonMark subset; no raw HTML reaches the DOM -->
        {@html renderMarkdown(summary.description_md)}
      </div>
    {/if}

    <!-- gallery -->
    {#if gallery.length}
      <div class="gallery">
        {#each gallery as g (g)}<img src={g} alt="" loading="lazy" />{/each}
      </div>
    {/if}

    <!-- resolver warnings -->
    {#if dep.missing.length || dep.cycles.length}
      <div class="card warns">
        <h3>{t('prev.warnings')}</h3>
        {#each dep.missing as m (m.from + m.requires)}
          <div class="warn-line mono">
            {m.from} {t('prev.requires')} <strong>{m.requires}</strong> — {t('prev.notInPack')}
          </div>
        {/each}
        {#each dep.cycles as c, i (i)}
          <div class="warn-line mono">{t('prev.cycle', { chain: c.join(' -> ') })}</div>
        {/each}
      </div>
    {/if}

    <!-- mods (libraries get their own collapsed section below) -->
    {#if ungroupedNonLib.length}
      <section>
        <h3 class="sec">{t('pe.mods')} <span class="faint">({ungroupedNonLib.length})</span></h3>
        <div class="rows">
          {#each ungroupedNonLib as m (m.filename)}
            <ModRow
              mod={m}
              enabled={enabled[m.filename] ?? (m.required || (m.default_enabled ?? true))}
              locked={m.required}
              onToggle={(on) => toggle(m.filename, on)}
              edges={dep.edgesBySource.get(m.filename) ?? []}
              missing={dep.missingBySource.get(m.filename) ?? []}
              conflicts={conflictsFor(m.filename)}
            />
          {/each}
        </div>
      </section>
    {/if}

    <!-- libraries (collapsed by default) -->
    {#if libraries.length}
      <section>
        <button class="sechead" class:open={libsOpen} onclick={() => (libsOpen = !libsOpen)}>
          <span class="caret"></span>{t('prev.libraries')} <span class="faint">({libraries.length})</span>
        </button>
        {#if libsOpen}
          <div class="rows" transition:unroll>
            {#each libraries as m (m.filename)}
              <ModRow
                mod={m}
                enabled={enabled[m.filename] ?? (m.required || (m.default_enabled ?? true))}
                locked={m.required}
                onToggle={(on) => toggle(m.filename, on)}
                edges={dep.edgesBySource.get(m.filename) ?? []}
                missing={dep.missingBySource.get(m.filename) ?? []}
                conflicts={conflictsFor(m.filename)}
              />
            {/each}
          </div>
        {/if}
      </section>
    {/if}

    <!-- assets -->
    {#if buckets}
      {#snippet assetList(items: AssetEntry[])}
        <div class="rows">
          {#each items as a (a.dest)}
            <div class="arow">
              <ModIcon
                name={a.dest.split('/').pop() ?? a.dest}
                iconUrl={a.display?.icon_url}
                source={a.source}
                sha1={a.sha1}
                size={28}
              />
              <span class="anm">{assetName(a)}</span>
              <span class="apath mono faint">{a.dest}</span>
              <span class="faint">{formatBytes(a.size_bytes)}</span>
            </div>
          {/each}
        </div>
      {/snippet}

      {#if buckets.resourcepacks.length}
        <section>
          <h3 class="sec">{t('prev.resourcePacks')} <span class="faint">({buckets.resourcepacks.length})</span></h3>
          {@render assetList(buckets.resourcepacks)}
        </section>
      {/if}
      {#if buckets.shaderpacks.length}
        <section>
          <h3 class="sec">{t('prev.shaderPacks')} <span class="faint">({buckets.shaderpacks.length})</span></h3>
          {@render assetList(buckets.shaderpacks)}
        </section>
      {/if}
      {#if buckets.configs.length}
        <section>
          <button class="sechead" class:open={configsOpen} onclick={() => (configsOpen = !configsOpen)}>
            <span class="caret"></span>{t('prev.configs')} <span class="faint">({buckets.configs.length})</span>
          </button>
          <!-- the wrapper exists because the list is a snippet three
               always-visible sections share, so the transition cannot go on
               its own root. Measured layout-neutral: with and without it the
               section and every row keep the same box. -->
          {#if configsOpen}
            <div transition:unroll>{@render assetList(buckets.configs)}</div>
          {/if}
        </section>
      {/if}
      {#if buckets.other.length}
        <section>
          <h3 class="sec">{t('prev.otherFiles')} <span class="faint">({buckets.other.length})</span></h3>
          {@render assetList(buckets.other)}
        </section>
      {/if}
    {/if}
  {/if}
</div>

<style>
  /* The literal blacks and whites below are deliberate and must not become
     theme tokens: this view renders what the launcher shows, and the launcher
     is dark whichever theme the panel is wearing. A preview that followed the
     panel would stop being a preview. */
  .preview {
    --p-bg: #121212;
    --p-surface: #1e1e1e;
    --p-surface-2: #262626;
    --p-fg: #eeeeee;
    --p-fg-dim: #b0b0b0;
    --p-accent: #bb86fc;
    --p-outline: #3a3a3a;
    --p-danger: #ff6b6b;
    --p-ok: #03dac6;
    background: var(--p-bg);
    color: var(--p-fg);
    border: 1px solid var(--seam-bright);
    border-radius: 10px;
    padding: 0 0 18px;
    overflow: hidden;
    font-size: var(--fs-md);
  }
  .ctl {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 16px;
    background: var(--p-surface);
    border-bottom: 1px solid var(--p-outline);
  }
  .ctl .ttl {
    font-weight: 600;
    color: var(--p-fg);
  }
  .ctl .sub {
    font-size: var(--fs-xs);
    color: var(--p-fg-dim);
  }
  .sp {
    flex: 1;
  }
  .ver {
    font-size: var(--fs-sm);
    color: var(--p-accent);
  }
  .refresh {
    background: transparent;
    border: 1px solid var(--p-outline);
    color: var(--p-fg);
    padding: 5px 14px;
    border-radius: 6px;
    cursor: pointer;
  }
  .refresh:hover:not(:disabled) {
    border-color: var(--p-accent);
  }
  .note {
    color: var(--p-fg-dim);
    margin: 14px 16px;
    font-size: var(--fs-sm);
  }
  .skeleton {
    margin: 14px 16px 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .sk {
    border-radius: 10px;
    background: linear-gradient(
      90deg,
      var(--p-surface) 25%,
      var(--p-surface-2) 37%,
      var(--p-surface) 63%
    );
    background-size: 400% 100%;
    animation: sk-pulse 1.3s ease-in-out infinite;
  }
  .sk-hero {
    height: 150px;
  }
  .sk-row {
    height: 46px;
  }
  /* The sweep itself is the product's, declared once in app.css. Only the two
     colours it runs between are this card's, because the preview paints in the
     pack's palette rather than the panel's. Scoping raises the specificity of
     the rule above past the global reduced-motion switch, so the switch is
     repeated here at the same weight -- a shimmer nobody asked to see is worse
     than no shimmer. */
  @media (prefers-reduced-motion: reduce) {
    .sk {
      animation: none;
    }
  }
  .checks {
    margin: 14px 16px 0;
    padding: 10px 14px;
    border: 1px solid var(--p-outline);
    border-radius: 8px;
    background: var(--p-surface);
    font-size: var(--fs-sm);
    line-height: 1.6;
  }
  .checks.bad {
    border-color: color-mix(in srgb, var(--p-danger) 55%, transparent);
  }
  .checks strong {
    color: var(--p-danger);
  }
  .checks ul {
    margin: 6px 0 0;
    padding-left: 18px;
  }
  .checks ul + span {
    display: inline-block;
    margin-top: 10px;
  }
  .diff {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
    margin: 14px 16px 0;
    padding: 8px 12px;
    border: 1px solid var(--p-outline);
    border-radius: 8px;
    background: var(--p-surface);
    font-size: var(--fs-sm);
  }
  .diff.clean {
    border-color: color-mix(in srgb, var(--p-ok) 35%, transparent);
  }
  .diff .add,
  .difflist .add {
    color: var(--p-ok);
  }
  .diff .rem,
  .difflist .rem {
    color: var(--p-danger);
  }
  .diff .chg,
  .difflist .chg {
    color: var(--p-accent);
  }
  .diff .ok {
    color: var(--p-ok);
  }
  .link {
    background: none;
    border: none;
    color: var(--p-accent);
    cursor: pointer;
    font-size: var(--fs-sm);
    padding: 0;
  }
  .difflist {
    margin: 6px 16px 0;
    padding: 10px 12px;
    border: 1px solid var(--p-outline);
    border-radius: 8px;
    background: var(--p-bg);
    font-size: var(--fs-sm);
    line-height: 1.7;
    max-height: 220px;
    overflow: auto;
  }
  .faint {
    opacity: 0.62;
  }

  .hero {
    margin: 14px 16px 0;
    min-height: 150px;
    border-radius: 12px;
    background: linear-gradient(135deg, #1e3a8a, #1d4ed8);
    background-size: cover;
    background-position: center;
    display: flex;
    align-items: flex-end;
    gap: 18px;
    padding: 20px 22px;
  }
  .heroicon img,
  .heroicon .avatar {
    width: 72px;
    height: 72px;
    border-radius: 14px;
    flex: none;
    object-fit: cover;
    box-shadow: 0 4px 18px rgba(0, 0, 0, 0.5);
  }
  .heroicon .avatar {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: var(--fs-3xl);
    font-weight: 700;
    color: #fff;
  }
  .herotext h1 {
    margin: 0;
    font-size: var(--fs-2xl);
    font-weight: 700;
    color: #fff;
  }
  .herotext .tag {
    margin: 5px 0 0;
    color: rgba(255, 255, 255, 0.88);
    font-size: var(--fs-md);
  }
  .metachips {
    display: flex;
    gap: 7px;
    flex-wrap: wrap;
    margin-top: 11px;
  }
  .metachips .mc {
    font-size: var(--fs-xs);
    padding: 2px 9px;
    border-radius: 999px;
    background: rgba(0, 0, 0, 0.4);
    color: #fff;
    border: 1px solid rgba(255, 255, 255, 0.18);
  }

  .card {
    margin: 14px 16px 0;
    padding: 14px 16px;
    background: var(--p-surface);
    border: 1px solid var(--p-outline);
    border-radius: 12px;
  }
  .about {
    line-height: 1.6;
    color: var(--p-fg-dim);
  }
  .about :global(h1),
  .about :global(h2),
  .about :global(h3) {
    color: var(--p-fg);
    margin: 0.6em 0 0.3em;
  }
  .about :global(a) {
    color: var(--p-accent);
  }
  .about :global(code) {
    background: var(--p-bg);
    padding: 1px 5px;
    border-radius: 4px;
    font-size: var(--fs-sm);
  }
  .about :global(pre) {
    background: var(--p-bg);
    padding: 10px 12px;
    border-radius: 8px;
    overflow: auto;
  }
  .about :global(img) {
    max-width: 100%;
    border-radius: 8px;
  }

  .gallery {
    display: flex;
    gap: 10px;
    overflow-x: auto;
    padding: 14px 16px 0;
  }
  .gallery img {
    height: 120px;
    border-radius: 8px;
    border: 1px solid var(--p-outline);
  }

  .warns {
    border-color: color-mix(in srgb, var(--p-danger) 45%, transparent);
  }
  .warns h3 {
    margin: 0 0 8px;
    font-size: var(--fs-sm);
    color: var(--p-danger);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .warn-line {
    font-size: var(--fs-sm);
    color: var(--p-fg);
    padding: 2px 0;
  }

  section {
    margin: 18px 16px 0;
  }
  .sec {
    font-size: var(--fs-sm);
    color: var(--p-fg-dim);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin: 0 0 9px;
  }
  .rows {
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
  .sechead {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    background: none;
    border: none;
    color: var(--p-fg-dim);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-size: var(--fs-sm);
    cursor: pointer;
    padding: 0 0 9px;
  }
  .sechead:hover {
    color: var(--p-fg);
  }
  .sechead .caret {
    width: 0;
    height: 0;
    border-top: 4px solid transparent;
    border-bottom: 4px solid transparent;
    border-left: 5px solid currentColor;
    transition: transform var(--dur-state) var(--ease-out);
  }
  .sechead.open .caret {
    transform: rotate(90deg);
  }
  .arow {
    display: flex;
    align-items: center;
    gap: 11px;
    padding: 7px 12px;
    border: 1px solid var(--p-outline);
    border-radius: 8px;
    background: var(--p-surface);
  }
  .arow .anm {
    font-size: var(--fs-md);
    color: var(--p-fg);
  }
  .arow .apath {
    font-size: var(--fs-xs);
  }
  .arow .faint:last-child {
    margin-left: auto;
  }
</style>
