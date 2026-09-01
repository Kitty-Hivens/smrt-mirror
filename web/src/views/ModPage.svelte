<script lang="ts">
  import { api, ApiError } from '../lib/api';
  import { detailOf, notifyFail, toasts } from '../lib/toasts.svelte';
  import { route } from '../lib/route.svelte';
  import { t } from '../lib/i18n.svelte';
  import { isDebug } from '../lib/roles';
  import type { JarDiff, ModDetail, ModEdge, Source, VersionRow } from '../lib/types';
  import ModIcon from './ModIcon.svelte';
  import Section from './ui/Section.svelte';
  import Skeleton from './ui/Skeleton.svelte';

  // The mod page: a public, read-only view of one mod -- identity, releases,
  // relations, and the packs that ship it. Reachable from the registry, a pack's
  // mod list, and the graph; the target mod is the shared route focus.
  let {
    modRef,
    me,
    onBack,
  }: {
    // a numeric mod id or a `sha1:<hash>` artifact reference
    modRef: string;
    me: { role: string } | null;
    onBack: () => void;
  } = $props();

  const canDebug = $derived(isDebug(me?.role));

  let detail = $state<ModDetail | null>(null);
  let loading = $state(true);


  // reload whenever the focused mod ref changes (deps navigate between mods)
  $effect(() => {
    const ref = modRef;
    loading = true;
    detail = null;
    api
      .modDetail(ref)
      .then((d) => {
        if (modRef === ref) detail = d;
      })
      .catch((e) => {
        if (modRef === ref) notifyFail(e);
      })
      .finally(() => {
        if (modRef === ref) loading = false;
      });
  });

  // a mod with any Modrinth-verified file: a self-hosted sibling under it reads as
  // a likely repackage (the same signal the registry management view uses)
  const modHasVerified = $derived(
    detail?.releases.some((r) => r.files.some((f) => f.modrinth_version_id)) ?? false,
  );

  // header icon source: a Modrinth project icon when known, else the first cached
  // jar's embedded icon, else the letter fallback
  const iconSource = $derived<Source>(
    detail?.modrinth_project_id
      ? { type: 'modrinth', project_id: detail.modrinth_project_id, version_id: '' }
      : ({ type: 'smrt_static', url: '' } as Source),
  );
  // any cached artifact's sha1 -- the embedded-icon leg of the chain, and the
  // whole icon when the mod has no Modrinth identity
  const iconSha1 = $derived(
    detail?.releases.flatMap((r) => r.files).find((f) => f.cached)?.sha1 ?? null,
  );

  // edge kind -> stroke colour, matching the graph legend
  const KIND_COLOR: Record<string, string> = {
    requires: 'var(--accent)',
    optional_dep: 'var(--fg-dim)',
    recommends: 'var(--fg-dim)',
    conflicts: 'var(--danger)',
    provides: 'var(--ok)',
  };
  const kindColor = (k: string) => KIND_COLOR[k] ?? 'var(--fg-dim)';

  // wide mods span many MC versions; collapse a long run to its bounds + a count
  function mcFacet(vs: string[]): { span: boolean; items: string[]; count: number } {
    if (vs.length <= 4) return { span: false, items: vs, count: vs.length };
    return { span: true, items: [vs[0], vs[vs.length - 1]], count: vs.length };
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

  function goMod(e: ModEdge) {
    if (e.other_mod_id != null) route.openMod(e.other_mod_id);
  }

  // repack (tamper) diff for a self-hosted file, operator-only; toggles an inline
  // panel like the registry management view
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
</script>

<div class="view">
  <button class="back mono" onclick={onBack}>&larr; {t('mod.back')}</button>


  {#if loading}
    <!-- the head, then the releases under it: the page's own shape rather than a
         line of text where a page is about to be -->
    <Skeleton rows={1} height={72} />
    <Skeleton rows={4} height={46} gap={0} shape="row" lead={22} />
  {:else if detail}
    <header class="head">
      <ModIcon name={detail.name} source={iconSource} sha1={iconSha1} size={52} mono />
      <div class="hinfo">
        <h1 class="hname">
          {detail.name}{#if detail.author}<span class="hby">{t('mm.by', { author: detail.author })}</span>{/if}
        </h1>
        <div class="hmeta">
          {#if detail.modid}<span class="mono modid">{detail.modid}</span>{/if}
          {#each detail.loaders as l}<span class="tag">{l}</span>{/each}
          {#if detail.mc_versions.length}
            {@const mc = mcFacet(detail.mc_versions)}
            <span class="facet">
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
          {#if detail.modrinth_project_id}<span class="chip verified">Modrinth</span>{/if}
        </div>
      </div>
    </header>

    <Section title={t('mod.releases')} count={detail.releases.length} flush>
      <div class="rels">
        {#each detail.releases as rel (rel.release_id)}
          <div class="rel">
            <div class="relhead">
              <span class="rver mono">{rel.version_number}</span>
              <span class="chip ch-{rel.channel}">{rel.channel}</span>
              <span class="faint mono">{t('mm.filesN', { n: rel.files.length })}</span>
            </div>
            {#each rel.files as f (f.sha1)}
              <div class="file">
                <ModIcon
                  name={f.filename ?? detail.name}
                  source={{ type: 'smrt_cache', sha1: f.sha1 }}
                  size={22}
                  mono
                />
                <div class="finfo">
                  <div class="fname">{f.filename ?? f.sha1.slice(0, 16)}</div>
                  <div class="fmeta muted mono">
                    {f.targets.join(', ')}{#if f.mc_versions.length} &middot; {f.mc_versions.join(', ')}{/if}
                    &middot; {fmtBytes(f.size_bytes)}{#if !f.cached} &middot; {t('mm.uncached')}{/if}
                  </div>
                </div>
                {#if f.modrinth_version_id}
                  <span class="chip verified" title="Modrinth-verified">{t('mm.verified')}</span>
                {:else if modHasVerified}
                  <span class="chip repack" title={t('mm.repackHint')}>{t('mm.repack')}</span>
                {:else}
                  <span class="chip">{t('mm.selfhost')}</span>
                {/if}
                {#if canDebug && !f.modrinth_version_id && modHasVerified && f.cached}
                  <button
                    class="link"
                    class:active={diffFor === f.sha1}
                    onclick={() => showDiff(f)}>{t('mm.diff')}</button>
                {/if}
              </div>
              {#if diffFor === f.sha1}
                <div class="diffpanel">
                  {#if diffLoading}
                    <Skeleton rows={3} height={18} gap={4} />
                  {:else if diffErr}
                    <div class="err mono">{diffErr}</div>
                  {:else if diffData}
                    <div class="diffsum mono">
                      {t('mm.diffClasses', { n: diffData.changed_classes.length })} &middot;
                      {t('mm.diffResources', { n: diffData.changed_resources.length })} &middot;
                      {t('mm.diffAdded', { n: diffData.added.length })} &middot;
                      {t('mm.diffRemoved', { n: diffData.removed.length })}
                    </div>
                    {#if diffData.changed_classes.length}
                      {#each diffData.changed_classes as c}<div class="mono diffrow">{c}</div>{/each}
                    {:else}
                      <div class="muted s">{t('mm.diffNoClasses')}</div>
                    {/if}
                  {/if}
                </div>
              {/if}
            {/each}
          </div>
        {/each}
        {#if detail.releases.length === 0}
          <div class="muted s pad">{t('mirror.noVersions')}</div>
        {/if}
      </div>
    </Section>

    <Section title={t('mod.deps')} count={detail.edges.length} flush>
      <div class="edges">
        {#each detail.edges as e, i (i)}
          <div class="edge">
            <span class="kind mono" style="--c:{kindColor(e.kind)}">{e.kind}</span>
            {#if e.dir === 'in'}
              {#if e.other_mod_id != null}
                <button class="modlink" onclick={() => goMod(e)}>{e.other_name}</button>
              {:else}<span class="ext">{e.other_name}</span>{/if}
              <span class="arrow mono" aria-hidden="true">&rarr;</span>
              <span class="self">{detail.name}</span>
            {:else}
              <span class="self">{detail.name}</span>
              <span class="arrow mono" aria-hidden="true">&rarr;</span>
              {#if e.other_mod_id != null}
                <button class="modlink" onclick={() => goMod(e)}>{e.other_name}</button>
              {:else}<span class="ext" title={t('mod.external')}>{e.other_name}</span>{/if}
            {/if}
          </div>
        {/each}
        {#if detail.edges.length === 0}
          <div class="muted s pad">{t('mod.noDeps')}</div>
        {/if}
      </div>
    </Section>

    <Section title={t('mod.usedBy')} count={detail.used_by.length} flush>
      <div class="uses">
        {#each detail.used_by as u (u.pack_id + u.pack_version)}
          <div class="use">
            <span class="upack">{u.pack_id}</span>
            <span class="uver mono faint">{u.pack_version}</span>
            <span class="grow"></span>
            <span class="ufile mono muted">{u.filename}</span>
          </div>
        {/each}
        {#if detail.used_by.length === 0}
          <div class="muted s pad">{t('mod.noUsedBy')}</div>
        {/if}
      </div>
    </Section>
  {:else if !loading}
    <div class="muted s">{t('mod.notFound')}</div>
  {/if}
</div>

<style>
  .view {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    max-width: 920px;
  }
  .back {
    align-self: flex-start;
    background: transparent;
    border: none;
    border-radius: 0;
    color: var(--fg-faint);
    font-size: var(--fs-sm);
    padding: 2px 4px;
  }
  .back:hover {
    color: var(--fg-dim);
  }
  .err {
    color: var(--danger);
    background: var(--danger-soft);
    border: 1px solid color-mix(in srgb, var(--danger) 40%, transparent);
    border-radius: var(--radius-sm);
    padding: var(--space-3) var(--space-4);
    font-size: var(--fs-sm);
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--space-4);
  }
  .hinfo {
    min-width: 0;
  }
  .hname {
    font-size: var(--fs-2xl);
    font-weight: 680;
    letter-spacing: -0.01em;
    display: flex;
    align-items: baseline;
    gap: 10px;
    flex-wrap: wrap;
  }
  .hby {
    font-size: var(--fs-md);
    font-weight: 400;
    color: var(--fg-faint);
  }
  .hmeta {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
    margin-top: 8px;
  }
  .modid {
    font-size: var(--fs-xs);
    color: var(--fg-dim);
    padding: 2px 8px;
    border: 1px solid var(--seam);
    border-radius: var(--radius-sm);
  }
  .facet {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .ell {
    color: var(--fg-faint);
    font-size: var(--fs-xs);
  }
  .fcount {
    font-size: var(--fs-xs);
    color: var(--fg-faint);
  }

  .rels {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
  }
  .rel {
    border-left: 2px solid var(--seam-bright);
    padding-left: var(--space-3);
  }
  .relhead {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 2px 0 4px;
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
  .link:hover,
  .link.active {
    color: var(--fg);
  }
  .diffpanel {
    margin: 2px 0 var(--space-2) 30px;
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
  .diffrow {
    font-size: var(--fs-xs);
    padding: 1px 0;
    overflow-wrap: anywhere;
  }

  .edges {
    display: flex;
    flex-direction: column;
  }
  .edge {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 9px var(--space-4);
    border-bottom: 1px solid var(--seam);
    font-size: var(--fs-md);
    flex-wrap: wrap;
  }
  .edge:last-child {
    border-bottom: none;
  }
  .kind {
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--c);
    border: 1px solid color-mix(in srgb, var(--c) 40%, var(--seam));
    border-radius: 999px;
    padding: 1px 8px;
    flex-shrink: 0;
  }
  .arrow {
    color: var(--fg-faint);
  }
  .self {
    color: var(--fg-dim);
  }
  .modlink {
    background: transparent;
    border: none;
    border-radius: 0;
    padding: 0;
    color: var(--fg);
    font-size: var(--fs-md);
    text-decoration: underline;
    text-decoration-color: var(--seam-bright);
    text-underline-offset: 2px;
    cursor: pointer;
  }
  .modlink:hover {
    text-decoration-color: var(--fg);
  }
  .ext {
    color: var(--fg-faint);
    font-family: var(--mono);
    font-size: var(--fs-sm);
  }

  .uses {
    display: flex;
    flex-direction: column;
  }
  .use {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 9px var(--space-4);
    border-bottom: 1px solid var(--seam);
    font-size: var(--fs-md);
  }
  .use:last-child {
    border-bottom: none;
  }
  .grow {
    flex: 1;
  }
  .uver {
    font-size: var(--fs-xs);
  }
  .ufile {
    font-size: var(--fs-xs);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 45%;
  }
  .s {
    font-size: var(--fs-sm);
  }
  .pad {
    padding: var(--space-4);
  }
</style>
