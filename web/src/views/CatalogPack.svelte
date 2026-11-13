<script lang="ts">
  import { api, ApiError } from '../lib/api';
  import { notifyFail } from '../lib/toasts.svelte';
  import { dialogs } from '../lib/dialogs.svelte';
  import { href, plainClick, route, type ModHint } from '../lib/route.svelte';
  import { i18n, t } from '../lib/i18n.svelte';
  import { mirror } from '../lib/mirror.svelte';
  import { terms } from '../lib/terms.svelte';
  import { renderMarkdown } from '../lib/markdown';
  import { inLanguage } from '../lib/cardtext';
  import ModIcon from './ModIcon.svelte';
  import PackThreads from './PackThreads.svelte';
  import Skeleton from './ui/Skeleton.svelte';
  import type { PackManifest, PackSummary } from '../lib/types';

  // One pack in the catalog, on a page of its own. The catalog is the showcase,
  // so the card opens into the pack's front: its banner, what it is, what it is
  // built on, and what is in it. It has an address, so a pack can be linked to
  // the way a mod can.
  let { packId, me }: { packId: string; me: { uid: number; login: string } | null } = $props();

  let summary = $state<PackSummary | null>(null);
  let manifest = $state<PackManifest | null>(null);
  let owner = $state<string | null>(null);
  let loading = $state(true);
  let missing = $state(false);
  let bannerBroken = $state(false);
  let iconBroken = $state(false);

  async function load(id: string) {
    loading = true;
    missing = false;
    bannerBroken = false;
    iconBroken = false;
    try {
      const s = await api.packSummary(id);
      if (id !== packId) return;
      summary = s;
      // A pack published once and never built has a card and no manifest. That
      // is a pack with nothing in it yet, not a failure of the page.
      const m = await api.manifest(id).catch((e) => {
        if (e instanceof ApiError && e.status === 404) return null;
        throw e;
      });
      if (id !== packId) return;
      manifest = m;
      if (s.tier === 'community') {
        const all = await api.community();
        if (id !== packId) return;
        owner = all.find((c) => c.summary.pack_id === id)?.owner_login ?? null;
      } else {
        owner = null;
      }
    } catch (e) {
      if (id !== packId) return;
      if (e instanceof ApiError && e.status === 404) missing = true;
      else notifyFail(e);
    } finally {
      if (id === packId) loading = false;
    }
  }

  // Loaded on arrival and again when a publish changes the card or the build
  // under it. Only a different pack clears the page: a refresh of the same one
  // keeps what is on screen until the newer answer lands.
  let shown: string | null = null;
  $effect(() => {
    void (mirror.packs + mirror.catalog);
    const id = packId;
    if (id !== shown) {
      shown = id;
      summary = null;
      manifest = null;
    }
    void load(id);
  });

  $effect(() => {
    if (summary) route.setCrumb(summary.display_name);
    return () => route.setCrumb(null);
  });

  const tagline = $derived(
    summary ? inLanguage(summary.tagline, summary.tagline_i18n, i18n.locale) : '',
  );
  const about = $derived(
    summary ? inLanguage(summary.description_md, summary.description_md_i18n, i18n.locale) : '',
  );
  const banner = $derived(summary?.banner_url && !bannerBroken ? summary.banner_url : null);
  const mods = $derived(manifest?.mods ?? []);

  type Mod = PackManifest['mods'][number];
  function modName(m: Mod): string {
    return m.display?.name ?? m.filename.replace(/\.jar$/i, '');
  }
  // What this page knows about a mod, handed to its page. A jar the registry
  // has not indexed has no page of its own to load, and this is all there is.
  function hint(m: Mod): ModHint {
    return {
      name: modName(m),
      filename: m.filename,
      size_bytes: m.size_bytes,
      source: m.source.type,
      pack_id: packId,
      pack_name: summary?.display_name,
    };
  }

  async function fork() {
    if (!me || !summary) return;
    if (!(await terms.ensure())) return;
    const name = (
      await dialogs.prompt(t('browse.forkPrompt'), {
        title: t('browse.fork'),
        initial: summary.pack_id.split('/').pop() ?? summary.pack_id,
      })
    )?.trim();
    if (!name) return;
    try {
      await api.fork(summary.pack_id, name);
      await dialogs.confirm(t('browse.forked', { name }), { title: t('browse.fork') });
    } catch (e) {
      notifyFail(e);
    }
  }
</script>

<div class="view">
  <a
    class="back mono"
    href="/browse"
    onclick={(e) => {
      if (!plainClick(e)) return;
      e.preventDefault();
      route.closeCatalog();
    }}>&larr; {t('nav.browse')}</a
  >

  {#if loading && !summary}
    <div class="sk hero-sk" aria-hidden="true"></div>
    <Skeleton rows={4} height={22} />
  {:else if summary}
    <header class="panel hero">
      {#if banner}
        <img class="cover" src={banner} alt="" onerror={() => (bannerBroken = true)} />
      {/if}
      <div class="heroin">
        <!-- the letter stands in for a missing icon only where there is no
             picture at all: under a banner it is one more square saying nothing -->
        {#if summary.icon_url && !iconBroken}
          <img class="picon" src={summary.icon_url} alt="" onerror={() => (iconBroken = true)} />
        {:else if !banner}
          <span class="picon letter mono" aria-hidden="true">
            {summary.display_name.slice(0, 1).toUpperCase()}
          </span>
        {/if}
        <div class="htext">
          <h1 class="hname">
            {summary.display_name}{#if summary.featured}<span class="feat mono">{t('packs.flag.featured')}</span>{/if}
          </h1>
          {#if owner}<div class="hby">{t('browse.by', { user: owner })}</div>{/if}
          {#if tagline}<p class="htag">{tagline}</p>{/if}
          <div class="chips">
            <span class="chip mono">{summary.minecraft_version}</span>
            {#if manifest}
              <span class="chip mono">{manifest.loader.name} {manifest.loader.version}</span>
              <span class="chip mono">Java {manifest.java.major}</span>
              <span class="chip">{t('prev.modsChip', { n: mods.length })}</span>
            {/if}
            <span class="chip mono">{summary.latest_pack_version}</span>
          </div>
        </div>
        {#if me}
          <button class="forkbtn" onclick={fork}>{t('browse.fork')}</button>
        {/if}
      </div>
    </header>

    {#if about}
      <!-- renderMarkdown sanitizes: the author's own markup is escaped, never passed through -->
      <div class="panel desc">{@html renderMarkdown(about)}</div>
    {/if}

    {#if summary.gallery_urls.length}
      <div class="gallery">
        {#each summary.gallery_urls as g (g)}
          <a class="shot" href={g} target="_blank" rel="noopener"><img src={g} alt="" loading="lazy" /></a>
        {/each}
      </div>
    {/if}

    <!-- What is being asked of this pack, where the pack is. A decision nobody
         can find is indistinguishable from one nobody made. Folded, so an empty
         discussion does not push the mod list down. -->
    <details class="panel talk">
      <summary>{t('browse.discussion')}</summary>
      <PackThreads packId={summary.pack_id} standalone canWrite={me != null} />
      {#if !me}
        <p class="muted signin">{t('browse.signInToReport')}</p>
      {/if}
    </details>

    {#if manifest}
      <section class="panel modsec">
        <h2 class="modhead">
          {t('catalog.mods')}<span class="count mono">{mods.length}</span>
        </h2>
        <div class="mods">
          {#each mods as m (m.sha1)}
            <a
              class="mrow"
              href={href.mod(`sha1:${m.sha1}`)}
              onclick={(e) => {
                if (!plainClick(e)) return;
                e.preventDefault();
                route.openMod(`sha1:${m.sha1}`, hint(m));
              }}
            >
              <ModIcon
                name={modName(m)}
                source={m.source}
                iconUrl={m.display?.icon_url ?? null}
                sha1={m.sha1}
                size={24}
                mono
              />
              <span class="mn">{modName(m)}</span>
              {#if !m.required}<span class="opt">{t('browse.optional')}</span>{/if}
            </a>
          {/each}
        </div>
      </section>
    {:else if !loading}
      <p class="muted">{t('catalog.noBuild')}</p>
    {/if}
  {:else if missing}
    <div class="gone">
      <div class="gtitle">{t('catalog.notFound')}</div>
      <p class="muted">{t('catalog.notFoundSub')}</p>
    </div>
  {/if}
</div>

<style>
  .view {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    max-width: 1080px;
  }
  .back {
    align-self: flex-start;
    color: var(--fg-faint);
    font-size: var(--fs-sm);
    text-decoration: none;
    padding: 2px 4px;
  }
  .back:hover {
    color: var(--fg-dim);
  }

  /* The banner is the pack's own picture, shown whole at its own proportions
     with the title under it rather than over it. A server banner carries its
     name painted into a corner, and text laid on top would land on that. */
  .hero {
    overflow: hidden;
  }
  .cover {
    display: block;
    width: 100%;
    height: auto;
    border-bottom: 1px solid var(--seam);
  }
  .heroin {
    display: flex;
    align-items: flex-end;
    gap: var(--space-4);
    padding: var(--space-4) var(--space-5) var(--space-5);
  }
  .picon {
    width: 64px;
    height: 64px;
    flex: none;
    border-radius: var(--radius-md);
    object-fit: cover;
  }
  .letter {
    display: grid;
    place-items: center;
    background: var(--panel-3);
    color: var(--fg-dim);
    font-size: var(--fs-2xl);
  }
  .htext {
    flex: 1;
    min-width: 0;
  }
  .hname {
    font-size: var(--fs-3xl);
    line-height: 1.15;
    overflow-wrap: anywhere;
  }
  .feat {
    margin-left: 10px;
    font-size: var(--fs-xs);
    font-weight: 400;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    vertical-align: middle;
    color: var(--accent-hue);
  }
  .hby {
    margin-top: 2px;
    font-size: var(--fs-sm);
    color: var(--fg-faint);
  }
  .htag {
    margin: 6px 0 0;
    font-size: var(--fs-lg);
    color: var(--fg-dim);
    max-width: 70ch;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: var(--space-3);
  }
  .chip {
    font-size: var(--fs-xs);
    padding: 2px 9px;
    border-radius: 999px;
    border: 1px solid var(--seam-bright);
    color: var(--fg-dim);
  }
  .forkbtn {
    flex: none;
    align-self: flex-end;
  }
  .hero-sk {
    height: 220px;
    border-radius: var(--radius-md);
  }

  .desc {
    padding: var(--space-4) var(--space-5);
    font-size: var(--fs-md);
    line-height: 1.6;
    color: var(--fg-dim);
  }
  .desc > :global(:first-child) {
    margin-top: 0;
  }
  .desc > :global(:last-child) {
    margin-bottom: 0;
  }
  .desc :global(h1),
  .desc :global(h2),
  .desc :global(h3) {
    color: var(--fg);
    margin: 1em 0 0.4em;
  }
  /* one measure for every block of the description, so a folded section
     does not run wider than the paragraphs around it */
  .desc > :global(*) {
    max-width: 75ch;
  }
  .desc :global(strong) {
    color: var(--fg);
  }
  .desc :global(img) {
    max-width: 100%;
    border-radius: var(--radius-sm);
  }
  /* A description's table is a spec sheet: horizontal seams only, and it
     scrolls on its own rather than pushing the column wider. */
  .desc :global(table) {
    width: 100%;
    border-collapse: collapse;
    margin: 0.7em 0;
    font-size: var(--fs-sm);
    display: block;
    overflow-x: auto;
  }
  .desc :global(th),
  .desc :global(td) {
    padding: 6px 10px;
    text-align: left;
    border-bottom: 1px solid var(--seam);
  }
  .desc :global(th) {
    color: var(--fg);
    font-weight: 600;
  }
  .desc :global(tbody tr:last-child td) {
    border-bottom: none;
  }
  .desc :global(details) {
    border: 1px solid var(--seam);
    border-radius: var(--radius-sm);
    padding: 8px 12px;
    margin: 0.7em 0;
  }
  .desc :global(details summary) {
    cursor: pointer;
    color: var(--fg);
    font-weight: 600;
  }

  .gallery {
    display: flex;
    gap: var(--space-3);
    overflow-x: auto;
    padding-bottom: 4px;
  }
  .shot {
    flex: none;
  }
  .shot img {
    display: block;
    height: 160px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--seam);
  }

  .talk {
    padding: var(--space-3) var(--space-4);
  }
  .talk summary {
    cursor: pointer;
    font-size: var(--fs-md);
    color: var(--fg-dim);
  }
  .talk[open] summary {
    margin-bottom: var(--space-3);
  }
  .signin {
    font-size: var(--fs-sm);
    margin: var(--space-2) 0 0;
  }

  .modsec {
    padding: var(--space-4);
  }
  .modhead {
    font-size: var(--fs-sm);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--fg-dim);
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    margin-bottom: var(--space-3);
  }
  .count {
    font-size: var(--fs-xs);
    color: var(--fg-faint);
    letter-spacing: 0;
  }
  .mods {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(230px, 1fr));
    gap: 2px var(--space-3);
  }
  .mrow {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    min-height: var(--control-min);
    padding: 4px 6px;
    border-radius: var(--radius-sm);
    color: inherit;
    text-decoration: none;
  }
  .mrow:hover {
    background: var(--panel-2);
  }
  .mn {
    flex: 1;
    min-width: 0;
    font-size: var(--fs-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .mrow:hover .mn {
    text-decoration: underline;
    text-decoration-color: var(--seam-bright);
    text-underline-offset: 2px;
  }
  /* a quiet word, not a shouted label: it shares the cell with the name, and
     the name is what is being looked for */
  .opt {
    flex: none;
    font-size: var(--fs-xs);
    color: var(--fg-faint);
  }

  .gone {
    padding: clamp(32px, 10vh, 96px) 0;
  }
  .gtitle {
    font-size: var(--fs-lg);
    font-weight: 600;
  }
  .gone p {
    margin: var(--space-2) 0 0;
  }

  /* A narrow column stacks the hero: the icon above the name, the fork button
     under the text, so the name gets the whole width. */
  @container view (max-width: 520px) {
    .heroin {
      flex-direction: column;
      align-items: flex-start;
      padding: var(--space-4);
      gap: var(--space-3);
    }
    .forkbtn {
      align-self: flex-start;
    }
    .hname {
      font-size: var(--fs-2xl);
    }
    .desc {
      padding: var(--space-3) var(--space-4);
    }
    .mods {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
