<script lang="ts">
  import { api, ApiError } from '../lib/api';
  import { notifyFail, toasts } from '../lib/toasts.svelte';
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
  import Glyph from './ui/Glyph.svelte';
  import { unroll } from '../lib/motion.svelte';
  import type { ManifestBuildInfo, PackManifest, PackSummary, Thread } from '../lib/types';

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
      versions = null;
      versionsOpen = false;
      notesOpen = false;
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
  const required = $derived(mods.filter((m) => m.required));
  const optional = $derived(mods.filter((m) => !m.required));
  const sizeBytes = $derived(
    mods.reduce((n, m) => n + m.size_bytes, 0) +
      (manifest?.assets ?? []).reduce((n, a) => n + a.size_bytes, 0),
  );

  function dateOf(rfc: string | undefined): string | null {
    if (!rfc) return null;
    const d = new Date(rfc);
    return Number.isNaN(d.getTime()) ? null : d.toLocaleDateString(i18n.locale);
  }
  const builtOn = $derived(dateOf(summary?.latest_built_at));

  function fmtSize(n: number): string {
    if (n >= 1e9) return `${(n / 1e9).toFixed(1)} ${t('catalog.gb')}`;
    if (n >= 1e6) return `${Math.round(n / 1e6)} ${t('catalog.mb')}`;
    return `${Math.max(1, Math.round(n / 1e3))} ${t('catalog.kb')}`;
  }

  // ── the control card ──

  // What changed in the build a player would install, in their language.
  const notes = $derived(
    manifest ? inLanguage(manifest.changelog, manifest.changelog_i18n, i18n.locale) : '',
  );
  let notesOpen = $state(false);
  // Whether the notes run past the folded height. A short note is shown whole
  // with nothing to unfold, rather than faded out under a button that does
  // nothing. The unfolded height is kept so opening eases to it.
  let notesEl = $state<HTMLDivElement | null>(null);
  let notesLong = $state(false);
  let notesFull = $state(0);
  $effect(() => {
    const el = notesEl;
    void notes;
    if (!el) return;
    const measure = () => {
      notesFull = el.scrollHeight;
      if (!el.classList.contains('open')) notesLong = el.scrollHeight > el.clientHeight + 1;
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  });

  // Every build, read only when somebody asks: most visitors want the newest.
  let versions = $state<ManifestBuildInfo[] | null>(null);
  let versionsOpen = $state(false);
  async function toggleVersions() {
    versionsOpen = !versionsOpen;
    if (!versionsOpen || versions) return;
    try {
      versions = (await api.manifestVersions(packId)).builds;
    } catch (e) {
      notifyFail(e);
      versionsOpen = false;
    }
  }

  // What is open in the discussion, so the card can say it before anyone
  // unfolds the thread list.
  let openThreads = $state<Thread[]>([]);
  $effect(() => {
    const id = packId;
    void mirror.packs;
    api
      .threads(id)
      .then((page) => {
        if (id === packId) openThreads = page.rows;
      })
      .catch(() => {
        // the list itself reports its own failure; the card says nothing
      });
  });
  let talkEl = $state<HTMLElement | null>(null);
  function showTalk() {
    talkEl?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  }

  // Whether this viewer may edit the pack, asked of the gate that decides it.
  let canEdit = $state(false);
  $effect(() => {
    const id = packId;
    canEdit = false;
    if (!me) return;
    api
      .myPackLevel(id)
      .then((r) => {
        if (id === packId) canEdit = r.level === 'own' || r.level === 'edit';
      })
      .catch(() => {});
  });
  const editSection = $derived(summary?.tier === 'community' ? 'mypacks' : 'packs');

  // The link button answers in place: its glyph turns into a tick for a
  // moment, which is the whole confirmation a copy needs.
  let copied = $state(false);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  async function copyLink() {
    const url = `${location.origin}${href.catalog(packId)}`;
    try {
      await navigator.clipboard.writeText(url);
      copied = true;
      clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => (copied = false), 1600);
    } catch {
      toasts.push({ kind: 'error', text: t('catalog.copyFailed'), detail: url });
    }
  }
  $effect(() => () => clearTimeout(copiedTimer));

  // The card wears the pack's own colour, taken from its banner, so Galaxy's
  // reads as Galaxy's. Sampled once from the picture already on screen; a
  // pack without a usable banner takes a hue from its name instead.
  let bannerHue = $state<number | null>(null);
  const nameHue = $derived(
    summary ? [...summary.display_name].reduce((h, c) => (h * 31 + c.charCodeAt(0)) >>> 0, 7) % 360 : 250,
  );
  const hue = $derived(bannerHue ?? nameHue);
  $effect(() => {
    void packId;
    bannerHue = null;
  });

  function sampleHue(img: HTMLImageElement) {
    try {
      const w = 64;
      const h = 14;
      const c = document.createElement('canvas');
      c.width = w;
      c.height = h;
      const g = c.getContext('2d', { willReadFrequently: true });
      if (!g) return;
      g.drawImage(img, 0, 0, w, h);
      const d = g.getImageData(0, 0, w, h).data;
      // a weighted mean of hue on the circle, counting only pixels with colour
      // in them: grey stone and black sky would otherwise pull every banner
      // to nothing in particular
      let x = 0;
      let y = 0;
      let total = 0;
      for (let i = 0; i < d.length; i += 4) {
        const r = d[i] / 255;
        const gr = d[i + 1] / 255;
        const b = d[i + 2] / 255;
        const max = Math.max(r, gr, b);
        const min = Math.min(r, gr, b);
        const l = (max + min) / 2;
        if (max === min || l < 0.12 || l > 0.9) continue;
        const sat = (max - min) / (1 - Math.abs(2 * l - 1));
        if (sat < 0.18) continue;
        let hh: number;
        if (max === r) hh = ((gr - b) / (max - min)) % 6;
        else if (max === gr) hh = (b - r) / (max - min) + 2;
        else hh = (r - gr) / (max - min) + 4;
        const rad = (hh * 60 * Math.PI) / 180;
        const weight = sat * (1 - Math.abs(l - 0.5) * 1.4);
        x += Math.cos(rad) * weight;
        y += Math.sin(rad) * weight;
        total += weight;
      }
      if (total < 1) return;
      bannerHue = Math.round(((Math.atan2(y, x) * 180) / Math.PI + 360) % 360);
    } catch {
      // a picture from another origin cannot be read back; the name's hue stands
    }
  }

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

    <!-- The card runs down the right edge from the top, level with the
         banner; the pack's own picture, words and mods fill the rest, and the
         discussion closes the page under them. -->
    <div class="layout">
      <header class="panel hero">
        {#if banner}
          <img
            class="cover"
            src={banner}
            alt=""
            onload={(e) => sampleHue(e.currentTarget as HTMLImageElement)}
            onerror={() => (bannerBroken = true)}
          />
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
          </div>
        </div>
      </header>

      <div class="lcol">
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

        {#if !manifest && !loading}
          <p class="muted">{t('catalog.noBuild')}</p>
        {/if}
      </div>

      <aside class="side">
        <section class="panel ctl" style="--pack-h:{hue}">
          <div class="cgrid">
          <div class="cb head">
            <div class="ver">
              <span class="vnum">{summary.latest_pack_version}</span>
              {#if summary.latest_channel}<span class="chan ch-{summary.latest_channel}">{summary.latest_channel}</span>{/if}
            </div>
            {#if builtOn}
              <div class="when"><Glyph name="clock" size={14} />{t('catalog.builtOn', { date: builtOn })}</div>
            {/if}
            <div class="actions">
              <div class="actrow">
                {#if me}
                  <button class="primary act grow" onclick={fork}><Glyph name="fork" />{t('browse.fork')}</button>
                {/if}
                <!-- alone on the line it says what it does; beside the fork
                     the glyph is enough -->
                <button
                  class="act linkbtn"
                  class:grow={!me}
                  class:done={copied}
                  onclick={copyLink}
                  title={copied ? t('catalog.linkCopied') : t('catalog.copyLink')}
                  aria-label={t('catalog.copyLink')}
                >
                  <span class="swapbox">
                    <span class="swap" class:on={!copied}><Glyph name="link" /></span>
                    <span class="swap" class:on={copied}><Glyph name="check" /></span>
                  </span>
                  {#if !me}{copied ? t('catalog.linkCopied') : t('catalog.copyLink')}{/if}
                </button>
                <span class="vh" aria-live="polite">{copied ? t('catalog.linkCopied') : ''}</span>
              </div>
              {#if canEdit}
                <a
                  class="btnlike act"
                  href={href.pack(packId, editSection)}
                  onclick={(e) => {
                    if (!plainClick(e)) return;
                    e.preventDefault();
                    route.openPackIn(editSection, packId);
                  }}><Glyph name="edit" />{t('catalog.openEditor')}</a
                >
              {/if}
            </div>
          </div>

          <dl class="cb specs">
            <div>
              <dt><Glyph name="cube" size={14} />Minecraft</dt>
              <dd>{summary.minecraft_version}</dd>
            </div>
            {#if manifest}
              <div>
                <dt><Glyph name="loader" size={14} />{t('catalog.loader')}</dt>
                <dd>{manifest.loader.name} {manifest.loader.version}</dd>
              </div>
              <div>
                <dt><Glyph name="java" size={14} />Java</dt>
                <dd>{manifest.java.major}</dd>
              </div>
            {/if}
          </dl>

          {#if manifest && mods.length}
            <div class="cb comp">
              <div class="comphead">
                <span>{t('prev.modsChip', { n: mods.length })}</span>
                <span class="size"><Glyph name="download" size={14} />{fmtSize(sizeBytes)}</span>
              </div>
              <div
                class="bar"
                role="img"
                aria-label={`${t('catalog.requiredN', { n: required.length })}, ${t('catalog.optionalN', { n: optional.length })}`}
              >
                <span class="req" style="width:{(required.length / mods.length) * 100}%"></span>
              </div>
              <div class="legend">
                <span><i class="sw req"></i>{t('catalog.requiredN', { n: required.length })}</span>
                <span><i class="sw opt"></i>{t('catalog.optionalN', { n: optional.length })}</span>
              </div>
            </div>
          {/if}

          {#if manifest}
            <div class="cb news">
              <div class="rowhead"><span class="badge"><Glyph name="notes" size={14} /></span>{t('catalog.whatsNew')}</div>
              {#if notes}
                <!-- renderMarkdown sanitizes: the author's own markup is escaped -->
                <div
                  class="notes"
                  class:open={notesOpen}
                  class:long={notesLong}
                  style={notesOpen ? `max-height:${notesFull}px` : undefined}
                  bind:this={notesEl}
                >
                  {@html renderMarkdown(notes)}
                </div>
                {#if notesLong}
                  <button class="link more" onclick={() => (notesOpen = !notesOpen)}>
                    {notesOpen ? t('catalog.showLess') : t('catalog.showAll')}
                  </button>
                {/if}
              {:else}
                <p class="none">{t('catalog.noNotes')}</p>
              {/if}
              <button class="disclose" class:open={versionsOpen} onclick={toggleVersions} aria-expanded={versionsOpen}>
                <Glyph name="chevron" size={14} />{t('catalog.allVersions')}
              </button>
              {#if versionsOpen && versions}
                <ol class="versions" transition:unroll>
                  {#each versions as v (v.version_number)}
                    <li>
                      <span class="vn">{v.version_number}</span>
                      <span class="chan ch-{v.version_type}">{v.version_type}</span>
                      <span class="vdate">{dateOf(v.date_published) ?? ''}</span>
                    </li>
                  {/each}
                </ol>
              {/if}
            </div>
          {/if}

          <button class="cb talkrow" onclick={showTalk}>
            <span class="badge" class:live={openThreads.length > 0}><Glyph name="chat" size={14} /></span>
            <span class="talktext">
              <span class="talktitle">{t('browse.discussion')}</span>
              <span class="talksub">
                {#if openThreads.length}
                  {t('catalog.openN', { n: openThreads.length })}: {openThreads[0].title}
                {:else}
                  {t('catalog.quiet')}
                {/if}
              </span>
            </span>
            <Glyph name="chevron" size={14} />
          </button>
          </div>
        </section>

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
        {/if}
      </aside>

      <!-- What is being asked of this pack, where the pack is. A decision
           nobody can find is indistinguishable from one nobody made, so it is
           a section of the page, not a fold at the bottom of one. -->
      <section class="panel talk" bind:this={talkEl}>
        <h2 class="talkhead">
          {t('browse.discussion')}{#if openThreads.length}<span class="count mono">{openThreads.length}</span>{/if}
        </h2>
        <PackThreads packId={summary.pack_id} standalone canWrite={me != null} />
      </section>
    </div>
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
    max-width: 1600px;
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
  /* One column on a narrow window, in reading order: the pack, what it runs
     on and what can be done with it, what it says, what is in it, what is
     being said about it. */
  .layout {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .hero {
    order: 0;
  }
  .ctl {
    order: 1;
  }
  .lcol {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    min-width: 0;
    order: 2;
  }
  .side {
    display: contents;
  }
  .modsec {
    order: 3;
  }
  .talk {
    order: 4;
  }

  /* Wide enough for the card to have the right edge to itself: it runs from
     the top, level with the banner, and stays in view while the page scrolls
     past it. Everything else is the column to its left, in reading order. */
  @container view (min-width: 1000px) {
    .layout {
      display: grid;
      grid-template-columns: minmax(0, 1fr) 320px;
      grid-template-areas:
        'hero card'
        'text card'
        'mods card'
        'talk card';
      grid-template-rows: auto auto auto 1fr;
      align-items: start;
    }
    .hero {
      grid-area: hero;
    }
    .lcol {
      grid-area: text;
    }
    .modsec {
      grid-area: mods;
    }
    .talk {
      grid-area: talk;
    }
    .ctl {
      grid-area: card;
      position: sticky;
      top: var(--space-4);
      max-height: calc(100dvh - 96px);
      overflow-y: auto;
    }
  }
  /* A large screen, which is most of them: the description and the mods
     side by side under the banner, the card down the right edge. */
  @container view (min-width: 1370px) {
    .layout {
      grid-template-columns: minmax(0, 46rem) minmax(260px, 1fr) 320px;
      grid-template-areas:
        'hero hero card'
        'text mods card'
        'talk talk card';
      grid-template-rows: auto auto 1fr;
    }
    /* The description sets the height of the row and the mods fill it,
       scrolling inside: a hundred mods in a column beside a short text would
       otherwise stretch the row and leave a hole under the text as tall as
       the list. contain:size keeps the list out of the row's measurement, and
       the floor keeps a short description from squeezing it shut. */
    .modsec {
      contain: size;
      align-self: stretch;
      min-height: 480px;
      display: flex;
      flex-direction: column;
    }
    .mods {
      flex: 1;
      min-height: 0;
      overflow-y: auto;
      /* the last rows fade, so a list cut by its own height says there is
         more under it */
      mask-image: linear-gradient(to bottom, black calc(100% - 28px), transparent);
      padding-bottom: 20px;
    }
  }
  /* ── the control card ──
     Its own colour is the pack's, from the banner, held to a lightness that
     reads on the panel's black and on paper alike. The blocks are separated
     by the card's own seam showing through a one-pixel gap, so the same rule
     draws the lines whether the blocks stack or sit two abreast. */
  .ctl {
    --pack-accent: hsl(var(--pack-h) 52% 68%);
    --pack-soft: hsl(var(--pack-h) 52% 68% / 0.14);
    --pack-line: hsl(var(--pack-h) 52% 68% / 0.55);
    container: ctl / inline-size;
    padding: 0;
    overflow: hidden;
    border-radius: var(--radius-lg);
    box-shadow:
      inset 0 3px 0 var(--pack-accent),
      var(--shadow-2);
  }
  :global(:root[data-theme='light']) .ctl {
    --pack-accent: hsl(var(--pack-h) 46% 36%);
    --pack-soft: hsl(var(--pack-h) 46% 36% / 0.12);
    --pack-line: hsl(var(--pack-h) 46% 36% / 0.5);
  }
  .cgrid {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 1px;
    background: var(--seam);
  }
  .cb {
    background: var(--panel);
    padding: var(--space-3) var(--space-4);
    min-width: 0;
  }
  .head {
    padding-top: var(--space-4);
  }
  /* given room, the same blocks sit two abreast: what it is and what to do
     beside what it runs on, what is in it beside what changed */
  @container ctl (min-width: 560px) {
    .cgrid {
      grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    }
    .talkrow {
      grid-column: 1 / -1;
    }
    .specs {
      align-content: center;
    }
  }

  .ver {
    display: flex;
    align-items: baseline;
    gap: var(--space-3);
  }
  .vnum {
    font-family: var(--mono);
    font-size: 26px;
    font-weight: 650;
    letter-spacing: -0.03em;
    line-height: 1;
    font-variant-numeric: tabular-nums;
  }
  .chan {
    font-family: var(--mono);
    font-size: var(--fs-xs);
    padding: 1px 8px;
    border-radius: 999px;
    border: 1px solid var(--seam-bright);
    color: var(--fg-dim);
  }
  .chan.ch-release {
    color: var(--pack-accent);
    border-color: var(--pack-line);
    background: var(--pack-soft);
  }
  .when {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: var(--space-2);
    font-size: var(--fs-sm);
    color: var(--fg-dim);
  }
  .actions {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    margin-top: var(--space-3);
  }
  .actrow {
    display: flex;
    gap: var(--space-2);
  }
  .act {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
    min-height: 36px;
    padding: 0 14px;
    font-size: var(--fs-md);
    text-decoration: none;
  }
  .act.grow {
    flex: 1;
  }
  /* the primary action wears the pack's colour */
  .act.primary {
    background: var(--pack-accent);
    color: #0b0b0c;
  }
  :global(:root[data-theme='light']) .act.primary {
    color: #fff;
  }
  /* beside the fork it is a square glyph; alone it takes the line and says
     what it does */
  .linkbtn:not(.grow) {
    width: 36px;
    padding: 0;
    flex: none;
  }
  .swapbox {
    position: relative;
    width: 16px;
    height: 16px;
    flex: none;
  }
  .linkbtn.done {
    color: var(--ok);
    border-color: color-mix(in srgb, var(--ok) 45%, var(--seam-bright));
  }
  /* the two glyphs share the button and trade places: the tick turns in as
     the link turns out, which is the confirmation */
  .swap {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    opacity: 0;
    transform: scale(0.6) rotate(-30deg);
    transition:
      opacity var(--dur-enter) var(--ease-out),
      transform var(--dur-layer) var(--ease-out-back);
  }
  .swap.on {
    opacity: 1;
    transform: none;
  }

  .specs {
    margin: 0;
    display: grid;
    gap: var(--space-3);
  }
  .specs div {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--space-4);
  }
  .specs dt {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--fs-md);
    color: var(--fg-dim);
  }
  .specs dd {
    margin: 0;
    font-family: var(--mono);
    font-size: var(--fs-md);
    text-align: right;
    overflow-wrap: anywhere;
  }

  .comphead {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: var(--fs-lg);
    font-weight: 650;
  }
  .size {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--fs-md);
    font-weight: 400;
    color: var(--fg-dim);
  }
  .bar {
    height: 10px;
    margin: var(--space-3) 0;
    border-radius: 999px;
    background: var(--panel-3);
    overflow: hidden;
  }
  /* fills once, as the page arrives: the proportion is the thing to read,
     and drawing it is how the eye is told so */
  .bar .req {
    display: block;
    height: 100%;
    background: var(--pack-accent);
    border-radius: 999px;
    transform-origin: left center;
    animation: fill calc(var(--dur-layer) * 3) var(--ease-out) calc(var(--dur-layer) * 0.8) both;
  }
  @keyframes fill {
    from {
      transform: scaleX(0);
    }
  }
  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1) var(--space-4);
    font-size: var(--fs-sm);
    color: var(--fg-dim);
  }
  .legend span {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .sw {
    width: 8px;
    height: 8px;
    border-radius: 2px;
  }
  .sw.req {
    background: var(--pack-accent);
  }
  .sw.opt {
    background: var(--panel-3);
    box-shadow: inset 0 0 0 1px var(--seam-bright);
  }

  /* a small square holding a block's glyph, so the card's sections read
     down its left edge */
  .badge {
    width: 26px;
    height: 26px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    background: var(--pack-soft);
    color: var(--pack-accent);
  }
  .rowhead {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    font-size: var(--fs-md);
    font-weight: 650;
    margin-bottom: var(--space-2);
  }
  .notes {
    font-size: var(--fs-md);
    line-height: 1.55;
    color: var(--fg-dim);
    max-height: calc(4 * 1.55em);
    overflow: hidden;
    transition: max-height var(--dur-layer) var(--ease-out);
  }
  .notes.long:not(.open) {
    mask-image: linear-gradient(to bottom, black 55%, transparent);
  }
  .notes :global(:first-child) {
    margin-top: 0;
  }
  .notes :global(:last-child) {
    margin-bottom: 0;
  }
  .notes :global(ul) {
    padding-left: 18px;
  }
  .none {
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--fg-dim);
  }
  .more {
    display: block;
    margin-top: var(--space-2);
    font-size: var(--fs-sm);
    color: var(--pack-accent);
  }
  .disclose {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    margin-top: var(--space-3);
    padding: 0;
    min-height: 0;
    background: none;
    border: none;
    box-shadow: none;
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--fg-dim);
  }
  .disclose:hover {
    background: none;
    color: var(--fg);
  }
  .disclose :global(.glyph) {
    transition: transform var(--dur-state) var(--ease-out);
  }
  .disclose.open :global(.glyph) {
    transform: rotate(90deg);
  }
  .versions {
    list-style: none;
    margin: var(--space-2) 0 0;
    padding: 0;
    font-size: var(--fs-sm);
  }
  .versions li {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    padding: 5px 0;
    border-top: 1px solid var(--seam);
  }
  .versions .vn {
    font-family: var(--mono);
    min-width: 7ch;
  }
  .vdate {
    margin-left: auto;
    color: var(--fg-dim);
    font-variant-numeric: tabular-nums;
  }

  /* the discussion as a row that leads somewhere: glyph, what it is, what is
     in it, and the arrow that says pressing goes there */
  .talkrow {
    width: 100%;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    border: none;
    border-radius: 0;
    box-shadow: none;
    text-align: left;
    color: var(--fg);
    min-height: 0;
    font-weight: 400;
  }
  .talkrow:hover {
    background: var(--panel-2);
  }
  .talkrow > :global(.glyph) {
    color: var(--fg-faint);
    transition: transform var(--dur-state) var(--ease-out);
  }
  .talkrow:hover > :global(.glyph) {
    transform: translateX(2px);
    color: var(--fg-dim);
  }
  .badge.live {
    color: var(--ok);
    background: var(--ok-soft);
  }
  .talktext {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .talktitle {
    font-size: var(--fs-lg);
    font-weight: 650;
  }
  .talksub {
    font-size: var(--fs-sm);
    color: var(--fg-dim);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
    padding: var(--space-4) var(--space-5) var(--space-5);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    min-width: 0;
  }
  .talkhead {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    font-size: var(--fs-xl);
    font-weight: 650;
    letter-spacing: -0.01em;
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
    grid-template-columns: repeat(auto-fill, minmax(210px, 1fr));
    align-content: start;
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

  /* A narrow column stacks the hero: the icon above the name, so the name gets
     the whole width. */
  @container view (max-width: 520px) {
    .heroin {
      flex-direction: column;
      align-items: flex-start;
      padding: var(--space-4);
      gap: var(--space-3);
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
