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
  import { stagger, unroll } from '../lib/motion.svelte';
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
  // The banner's own proportions, so the hero is laid out at its final height
  // from the first frame instead of growing when the picture lands and
  // pushing everything under it down.
  let bannerRatio = $state<number | null>(null);

  /// Fetch the banner ahead of drawing the page. It is read alongside the
  /// build, so it adds no wait of its own; a picture slower than that is not
  /// waited for past a moment, and the page takes the shift rather than the
  /// delay.
  function probeBanner(url: string): Promise<HTMLImageElement | null> {
    return new Promise((resolve) => {
      const img = new Image();
      const done = (v: HTMLImageElement | null) => {
        clearTimeout(timer);
        resolve(v);
      };
      const timer = setTimeout(() => done(null), 1200);
      img.onload = () => done(img.naturalWidth > 0 ? img : null);
      img.onerror = () => done(null);
      img.src = url;
    });
  }

  async function load(id: string) {
    loading = true;
    missing = false;
    bannerBroken = false;
    iconBroken = false;
    try {
      const s = await api.packSummary(id);
      if (id !== packId) return;
      // The build, the owner and the banner are read together and the page is
      // drawn once, whole: drawn as each arrived, it would rearrange itself
      // three times under the reader.
      const [m, own, img] = await Promise.all([
        // A pack published once and never built has a card and no manifest.
        // That is a pack with nothing in it yet, not a failure of the page.
        api.manifest(id).catch((e) => {
          if (e instanceof ApiError && e.status === 404) return null;
          throw e;
        }),
        s.tier === 'community'
          ? api.community().then((all) => all.find((c) => c.summary.pack_id === id)?.owner_login ?? null)
          : Promise.resolve(null),
        s.banner_url ? probeBanner(s.banner_url) : Promise.resolve(null),
      ]);
      if (id !== packId) return;
      bannerRatio = img ? img.naturalWidth / img.naturalHeight : null;
      if (img) sampleHue(img);
      summary = s;
      manifest = m;
      owner = own;
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
      modsAll = false;
      bannerRatio = null;
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
  // Whether the pack has anything to say for itself beside its card. A pack
  // with no description leaves no column for one, and its mods take the room.
  const hasText = $derived(
    !!about || (summary?.gallery_urls.length ?? 0) > 0 || (!manifest && !loading),
  );
  type Mod = PackManifest['mods'][number];
  function modName(m: Mod): string {
    return m.display?.name ?? m.filename.replace(/\.jar$/i, '');
  }
  // What a mod is to a player installing the pack. A mod the client has no use
  // for never reaches the player's install, so it is the server's, whatever the
  // lock on it says; one that ships switched off is not the same promise as
  // one that ships on.
  type ModState = 'required' | 'optional' | 'off' | 'server';
  function modState(m: Mod): ModState {
    if (m.env?.client === 'unsupported') return 'server';
    if (m.required) return 'required';
    return m.default_enabled === false ? 'off' : 'optional';
  }
  // By name as a reader says it: a jar's leading "+", its capitals and its
  // version digits are not where anybody looks for it.
  const mods = $derived.by(() => {
    const by = new Intl.Collator(i18n.locale, { sensitivity: 'base', ignorePunctuation: true, numeric: true });
    return [...(manifest?.mods ?? [])].sort((a, b) => by.compare(modName(a), modName(b)));
  });
  const STATES: ModState[] = ['required', 'optional', 'off', 'server'];
  const counts = $derived(
    Object.fromEntries(STATES.map((s) => [s, mods.filter((m) => modState(m) === s).length])) as Record<
      ModState,
      number
    >,
  );
  const STATE_LABEL: Record<ModState, 'catalog.requiredN' | 'catalog.optionalN' | 'catalog.offN' | 'catalog.serverN'> =
    { required: 'catalog.requiredN', optional: 'catalog.optionalN', off: 'catalog.offN', server: 'catalog.serverN' };
  const ROW_LABEL: Record<Exclude<ModState, 'required'>, 'browse.optional' | 'catalog.modOff' | 'catalog.modServer'> = {
    optional: 'browse.optional',
    off: 'catalog.modOff',
    server: 'catalog.modServer',
  };
  // On a narrow screen the list folds after a screenful, so the discussion
  // under it is not a hundred rows away.
  const MODS_FOLDED = 12;
  let modsAll = $state(false);
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

  // Every build, read only when somebody reaches for it: most visitors want the
  // newest. A pointer on its way to the button starts the read, so the list is
  // usually in hand by the time the click lands.
  let versions = $state<ManifestBuildInfo[] | null>(null);
  let versionsOpen = $state(false);
  let versionsRead: Promise<void> | null = null;
  function readVersions(): Promise<void> {
    const id = packId;
    versionsRead ??= api
      .manifestVersions(id)
      .then((r) => {
        if (id === packId) versions = r.builds;
      })
      .finally(() => (versionsRead = null));
    return versionsRead;
  }
  async function toggleVersions() {
    versionsOpen = !versionsOpen;
    if (!versionsOpen || versions) return;
    try {
      await readVersions();
    } catch (e) {
      notifyFail(e);
      versionsOpen = false;
    }
  }
  function prefetchVersions() {
    if (!versions) readVersions().catch(() => {});
  }

  // The words a player reads for what the mirror stores as vocabulary.
  const CHANNEL_KEY = {
    release: 'catalog.ch.release',
    beta: 'catalog.ch.beta',
    alpha: 'catalog.ch.alpha',
  } as const;
  function channelLabel(c: string): string {
    return c in CHANNEL_KEY ? t(CHANNEL_KEY[c as keyof typeof CHANNEL_KEY]) : c;
  }
  const LOADER_NAME: Record<string, string> = {
    forge: 'Forge',
    neoforge: 'NeoForge',
    cleanroom: 'Cleanroom',
    fabric: 'Fabric',
    quilt: 'Quilt',
  };

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
  // The row in the card goes to the discussion and takes the keyboard with it,
  // so the next Tab lands in the discussion rather than back in the card.
  let talkEl = $state<HTMLElement | null>(null);
  let talkHead = $state<HTMLElement | null>(null);
  function showTalk() {
    talkHead?.focus({ preventScroll: true });
    talkEl?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  }

  // The card stays in view while the page scrolls, and must fit the box that
  // scrolls rather than the window: the shell's top bar takes part of the
  // window, and a card measured against the window runs under its bottom edge.
  let layoutEl = $state<HTMLElement | null>(null);
  let viewH = $state(0);
  $effect(() => {
    let scroller: HTMLElement | null = null;
    for (let p = layoutEl?.parentElement ?? null; p; p = p.parentElement) {
      const o = getComputedStyle(p).overflowY;
      if (o === 'auto' || o === 'scroll') {
        scroller = p;
        break;
      }
    }
    if (!scroller) return;
    const el = scroller;
    const ro = new ResizeObserver(() => (viewH = el.clientHeight));
    ro.observe(el);
    return () => ro.disconnect();
  });

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

{#snippet back()}
  <a
    class="back mono"
    href="/browse"
    onclick={(e) => {
      if (!plainClick(e)) return;
      e.preventDefault();
      route.closeCatalog();
    }}>&larr; {t('nav.browse')}</a
  >
{/snippet}

<div class="view">
  {#if loading && !summary}
    {@render back()}
    <div class="sk hero-sk" aria-hidden="true"></div>
    <Skeleton rows={4} height={22} />
  {:else if summary}
    <!-- The card runs down the right edge from the very top, level with the
         way back; the pack's own picture, words and mods fill the rest, and
         the discussion closes the page under them. -->
    <div
      class="layout"
      class:bare={!hasText}
      style="--pack-h:{hue}{viewH ? `;--view-h:${viewH}px` : ''}"
      bind:this={layoutEl}
    >
      {@render back()}

      <header class="panel hero rise" style="--i:0">
        {#if banner}
          <img
            class="cover"
            src={banner}
            alt=""
            style={bannerRatio ? `aspect-ratio:${bannerRatio}` : undefined}
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

      {#if hasText}
        <div class="lcol rise" style="--i:2">
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
      {/if}

      <aside class="side">
        <section class="panel ctl">
          <div class="cgrid" class:full={!!manifest}>
          <div class="cb head rise" style="--i:1">
            <div class="ver">
              <span class="vnum">{summary.latest_pack_version}</span>
              {#if summary.latest_channel}<span class="chan ch-{summary.latest_channel}">{channelLabel(summary.latest_channel)}</span>{/if}
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

          <dl class="cb specs rise" style="--i:2">
            <div>
              <dt><Glyph name="cube" size={14} />Minecraft</dt>
              <dd>{summary.minecraft_version}</dd>
            </div>
            {#if manifest}
              <div>
                <dt><Glyph name="loader" size={14} />{t('catalog.loader')}</dt>
                <dd>{LOADER_NAME[manifest.loader.name] ?? manifest.loader.name} {manifest.loader.version}</dd>
              </div>
              <div>
                <dt><Glyph name="java" size={14} />Java</dt>
                <dd>{manifest.java.major}</dd>
              </div>
            {/if}
          </dl>

          {#if manifest && mods.length}
            <div class="cb comp rise" style="--i:3">
              <div class="comphead">
                <span>{t('prev.modsChip', { n: mods.length })}</span>
                <span class="size"><Glyph name="download" size={14} />{fmtSize(sizeBytes)}</span>
              </div>
              <div
                class="bar"
                role="img"
                aria-label={STATES.filter((s) => counts[s]).map((s) => t(STATE_LABEL[s], { n: counts[s] })).join(', ')}
              >
                {#each STATES as s (s)}
                  {#if counts[s]}<span class="seg s-{s}" style="flex-grow:{counts[s]}"></span>{/if}
                {/each}
              </div>
              <div class="legend">
                {#each STATES as s (s)}
                  {#if counts[s]}<span><i class="sw s-{s}"></i>{t(STATE_LABEL[s], { n: counts[s] })}</span>{/if}
                {/each}
              </div>
            </div>
          {/if}

          {#if manifest}
            <div class="cb news rise" style="--i:4">
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
              <button
                class="disclose"
                class:open={versionsOpen}
                class:busy={versionsOpen && !versions}
                onclick={toggleVersions}
                onpointerenter={prefetchVersions}
                onfocus={prefetchVersions}
                aria-expanded={versionsOpen}
                aria-busy={versionsOpen && !versions}
              >
                <Glyph name="chevron" size={14} />{t('catalog.allVersions')}
              </button>
              {#if versionsOpen && versions}
                <ol class="versions" transition:unroll>
                  {#each versions as v (v.version_number)}
                    <li>
                      <span class="vn">{v.version_number}</span>
                      <span class="chan ch-{v.version_type}">{channelLabel(v.version_type)}</span>
                      <span class="vdate">{dateOf(v.date_published) ?? ''}</span>
                    </li>
                  {/each}
                </ol>
              {/if}
            </div>
          {/if}

          <button class="cb talkrow rise" style="--i:5" onclick={showTalk}>
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
          <section class="panel modsec rise" style="--i:3">
            <h2 class="sechead">
              {t('catalog.mods')}<span class="count mono">{mods.length}</span>
            </h2>
            <div class="mods" class:all={modsAll}>
              {#each mods as m, i (m.sha1)}
                {@const st = modState(m)}
                <a
                  class="mrow"
                  use:stagger={i}
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
                  {#if st !== 'required'}<span class="opt st-{st}">{t(ROW_LABEL[st])}</span>{/if}
                </a>
              {/each}
            </div>
            {#if mods.length > MODS_FOLDED}
              <button class="disclose modsmore" class:open={modsAll} onclick={() => (modsAll = !modsAll)} aria-expanded={modsAll}>
                <Glyph name="chevron" size={14} />{modsAll ? t('catalog.showLess') : t('catalog.allMods', { n: mods.length })}
              </button>
            {/if}
          </section>
        {/if}
      </aside>

      <!-- What is being asked of this pack, where the pack is. A decision
           nobody can find is indistinguishable from one nobody made, so it is
           a section of the page, not a fold at the bottom of one. -->
      <section class="panel talk rise" style="--i:4" bind:this={talkEl}>
        <h2 class="sechead" tabindex="-1" bind:this={talkHead}>
          {t('browse.discussion')}{#if openThreads.length}<span class="count mono">{openThreads.length}</span>{/if}
        </h2>
        <PackThreads packId={summary.pack_id} standalone canWrite={me != null} />
      </section>
    </div>
  {:else if missing}
    {@render back()}
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
    justify-self: start;
    color: var(--fg-faint);
    font-size: var(--fs-sm);
    text-decoration: none;
    padding: 2px 4px;
    border-radius: var(--radius-sm);
    transition: color var(--dur-state) var(--ease-out);
  }
  .back:hover {
    color: var(--fg-dim);
  }
  .back:focus-visible {
    outline: none;
    box-shadow: var(--ring);
  }

  /* ── arrival ──
     The page's one orchestrated moment: the banner settles, then the card's
     blocks and the columns rise into place in reading order, the bar draws
     itself and the mods follow. Each step is short and the whole is over in
     about half a second. Under reduced motion the duration tokens are zero, and
     with them every delay here, so everything is simply present. */
  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(10px);
    }
  }
  .rise {
    animation: rise calc(var(--dur-layer) * 1.5) var(--ease-out) backwards;
    animation-delay: calc(var(--i, 0) * var(--dur-state) * 0.7);
  }
  @keyframes settle-in {
    from {
      opacity: 0;
      transform: scale(1.03);
    }
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
    animation: settle-in calc(var(--dur-layer) * 2.5) var(--ease-out) backwards;
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
     being said about it. The pack's colour is set here, on the whole page,
     because the mods and the discussion answer a pointer in it too. */
  .layout {
    --pack-accent: hsl(var(--pack-h) 52% 68%);
    --pack-soft: hsl(var(--pack-h) 52% 68% / 0.14);
    --pack-line: hsl(var(--pack-h) 52% 68% / 0.55);
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  :global(:root[data-theme='light']) .layout {
    --pack-accent: hsl(var(--pack-h) 46% 36%);
    --pack-soft: hsl(var(--pack-h) 46% 36% / 0.12);
    --pack-line: hsl(var(--pack-h) 46% 36% / 0.5);
  }
  .hero {
    order: 0;
  }
  .ctl {
    order: 1;
  }
  /* The description comes before the card in the markup and after it on a
     narrow screen. The browser keeps the first element on screen in markup
     order where it was when something above it grows, so with the description
     as that element, unfolding the versions in the card scrolled the page and
     threw the button off the top. Left out of that choice, the card's own
     content is what holds still. */
  .lcol {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    min-width: 0;
    order: 2;
    overflow-anchor: none;
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

  /* ── the control card ──
     Its own colour is the pack's, from the banner, held to a lightness that
     reads on the panel's black and on paper alike. The blocks are separated
     by the card's own seam showing through a one-pixel gap, so the same rule
     draws the lines whether the blocks stack or sit two abreast. The colour
     runs along the top edge as a border: a shadow drawn inside the card would
     sit under the blocks' own backgrounds and never be seen. */
  .ctl {
    container: ctl / inline-size;
    padding: 0;
    overflow: hidden;
    border-radius: var(--radius-lg);
    border-top: 3px solid var(--pack-accent);
    box-shadow: var(--shadow-2);
  }

  /* Wide enough for the card to have the right edge to itself: it runs from
     the very top, level with the way back, and stays in view while the page
     scrolls past it. It is held to the height of the box that scrolls, and
     what does not fit scrolls inside it. Everything else is the column to its
     left, in reading order. */
  @container view (min-width: 1000px) {
    .layout {
      display: grid;
      grid-template-columns: minmax(0, 1fr) 320px;
      grid-template-areas:
        'back card'
        'hero card'
        'text card'
        'mods card'
        'talk card';
      grid-template-rows: auto auto auto auto 1fr;
      align-items: start;
    }
    .layout.bare {
      grid-template-areas:
        'back card'
        'hero card'
        'mods card'
        'talk card';
      grid-template-rows: auto auto auto 1fr;
    }
    .back {
      grid-area: back;
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
    /* A sticky offset counts from inside the scrolling box's padding, which
       the shell sets to --space-5, so the card sticks at 0: exactly where it
       starts. An offset of its own would push it down from the start and let
       it jump back up as soon as it grew to fill its area. It is held to the
       height inside that padding. */
    .ctl {
      grid-area: card;
      position: sticky;
      top: 0;
      max-height: calc(var(--view-h, 100dvh) - 2 * var(--space-5));
      overflow-y: auto;
      scrollbar-width: thin;
    }
    .modsec .modsmore {
      display: none;
    }
  }
  /* A large screen, which is most of them: the description and the mods
     side by side under the banner, the card down the right edge. */
  @container view (min-width: 1370px) {
    .layout {
      grid-template-columns: minmax(0, 46rem) minmax(260px, 1fr) 320px;
      grid-template-areas:
        'back back card'
        'hero hero card'
        'text mods card'
        'talk talk card';
      grid-template-rows: auto auto auto 1fr;
    }
    .layout.bare {
      grid-template-areas:
        'back back card'
        'hero hero card'
        'mods mods card'
        'talk talk card';
    }
    /* The description sets the height of the row and the mods fill it,
       scrolling inside: a hundred mods in a column beside a short text would
       otherwise stretch the row and leave a hole under the text as tall as
       the list. contain:size keeps the list out of the row's measurement, and
       the floor keeps a short description from squeezing it shut. A pack with
       no description has no row to match, and its list runs at full length. */
    .layout:not(.bare) .modsec {
      contain: size;
      align-self: stretch;
      min-height: 480px;
      display: flex;
      flex-direction: column;
    }
    .layout:not(.bare) .mods {
      flex: 1;
      min-height: 0;
      overflow-y: auto;
      scrollbar-width: thin;
      /* the last rows fade, so a list cut by its own height says there is
         more under it */
      mask-image: linear-gradient(to bottom, black calc(100% - 28px), transparent);
      padding-bottom: 20px;
    }
  }
  /* A single column folds the list after a screenful (MODS_FOLDED rows), so
     the discussion is not a hundred rows below the fold. */
  @container view (max-width: 999.98px) {
    .mods:not(.all) > .mrow:nth-child(n + 13) {
      display: none;
    }
    /* In a single column the card is part of the page, and fifty builds
       unfolded in it would push everything under it a screen and a half down.
       The list keeps to about ten rows and scrolls inside. */
    .versions {
      max-height: 18rem;
      overflow-y: auto;
      scrollbar-width: thin;
      overscroll-behavior: contain;
    }
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
  /* Given room, the blocks sit two abreast: what it is, what it runs on and
     what is in it down the left, what changed down the right beside them,
     however long the notes run. Nothing is left as an empty cell, which the
     seam behind the blocks would paint as a grey slab. */
  @container ctl (min-width: 560px) {
    .cgrid {
      grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    }
    .cgrid.full {
      grid-template-areas:
        'head news'
        'specs news'
        'comp news'
        'talk talk';
      grid-template-rows: auto auto 1fr auto;
    }
    .cgrid.full .head {
      grid-area: head;
    }
    .cgrid.full .specs {
      grid-area: specs;
    }
    .cgrid.full .comp {
      grid-area: comp;
    }
    .cgrid.full .news {
      grid-area: news;
    }
    .cgrid:not(.full) .news,
    .talkrow {
      grid-column: 1 / -1;
    }
    .cgrid.full .talkrow {
      grid-area: talk;
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
  /* the primary action wears the pack's colour, and lifts under the pointer */
  .act.primary {
    background: var(--pack-accent);
    color: #0b0b0c;
    transition:
      filter var(--dur-state) var(--ease-out),
      transform var(--dur-state) var(--ease-out);
  }
  .act.primary:hover {
    opacity: 1;
    filter: brightness(1.1) saturate(1.1);
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
    align-content: center;
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

  .comp {
    align-content: center;
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
  /* What the pack asks of a player, in proportion: locked, on but switchable,
     shipped switched off, and the server's alone. Drawn once as the page
     arrives, left to right, because the proportion is the thing to read. */
  .bar {
    display: flex;
    gap: 2px;
    height: 10px;
    margin: var(--space-3) 0;
    border-radius: 999px;
    overflow: hidden;
    animation: draw calc(var(--dur-layer) * 3) var(--ease-out) calc(var(--dur-layer) * 0.8) backwards;
  }
  @keyframes draw {
    from {
      clip-path: inset(0 100% 0 0);
    }
    to {
      clip-path: inset(0 0 0 0);
    }
  }
  .seg {
    flex-basis: 0;
    min-width: 3px;
  }
  .s-required {
    background: var(--pack-accent);
  }
  .s-optional {
    background: color-mix(in srgb, var(--pack-accent) 45%, transparent);
  }
  .s-off {
    background: var(--seam-bright);
  }
  .s-server {
    background: repeating-linear-gradient(135deg, var(--seam-bright) 0 2px, transparent 2px 5px);
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
    transition:
      background var(--dur-enter) var(--ease-out),
      color var(--dur-enter) var(--ease-out);
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
    padding: 2px 4px;
    margin-left: -4px;
    min-height: 0;
    background: none;
    border: none;
    border-radius: var(--radius-sm);
    box-shadow: none;
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--fg-dim);
  }
  .disclose:hover {
    background: none;
    color: var(--fg);
  }
  .disclose:focus-visible {
    box-shadow: var(--ring);
  }
  .disclose :global(.glyph) {
    transition: transform var(--dur-enter) var(--ease-out);
  }
  .disclose.open :global(.glyph) {
    transform: rotate(90deg);
  }
  /* opened before the list has arrived: the chevron breathes until it does */
  @keyframes breathe {
    to {
      opacity: 0.3;
    }
  }
  .disclose.busy :global(.glyph) {
    animation: breathe 0.6s ease-in-out infinite alternate;
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
  /* drawn inside the row: the card clips anything drawn outside its edge */
  .talkrow:focus-visible {
    box-shadow: inset 0 0 0 2px var(--pack-line);
  }
  .talkrow > :global(.glyph) {
    color: var(--fg-faint);
    transition:
      transform var(--dur-enter) var(--ease-out),
      color var(--dur-state) var(--ease-out);
  }
  .talkrow:hover > :global(.glyph),
  .talkrow:focus-visible > :global(.glyph) {
    transform: translateX(3px);
    color: var(--pack-accent);
  }
  .talkrow:hover .badge {
    background: var(--pack-accent);
    color: var(--panel);
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

  /* one heading for every section the page holds */
  .sechead {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    margin: 0;
    font-size: var(--fs-xl);
    font-weight: 650;
    letter-spacing: -0.01em;
  }
  /* a place the card's row lands the keyboard, not a control of its own */
  .sechead:focus {
    outline: none;
  }
  .count {
    font-size: var(--fs-sm);
    font-weight: 400;
    color: var(--fg-faint);
    letter-spacing: 0;
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
    padding: var(--space-4) var(--space-5);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    min-width: 0;
  }

  .modsec {
    padding: var(--space-4);
  }
  .modsec .sechead {
    margin: 0 0 var(--space-3) 6px;
  }
  .mods {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(210px, 1fr));
    align-content: start;
    gap: 2px var(--space-3);
  }
  /* the rows follow the card in, a dozen in sequence and the rest together */
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
    transition: background var(--dur-state) var(--ease-out);
    animation: row-in var(--dur-enter) var(--ease-out) backwards;
    animation-delay: calc(var(--dur-layer) + var(--stagger, 0ms));
  }
  .mrow:hover {
    background: var(--panel-2);
  }
  /* drawn inside the row, so a list that scrolls does not cut it */
  .mrow:focus-visible {
    outline: none;
    box-shadow: inset 0 0 0 2px var(--pack-line);
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
  /* A quiet word with the bar's own mark in front of it: it shares the cell
     with the name, and the name is what is being looked for. */
  .opt {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: var(--fs-xs);
    color: var(--fg-faint);
  }
  .opt::before {
    content: '';
    width: 6px;
    height: 6px;
    border-radius: 2px;
  }
  .opt.st-optional::before {
    background: color-mix(in srgb, var(--pack-accent) 45%, transparent);
  }
  .opt.st-off {
    color: var(--fg-dim);
  }
  .opt.st-off::before {
    background: var(--seam-bright);
  }
  .opt.st-server::before {
    background: repeating-linear-gradient(135deg, var(--seam-bright) 0 1px, transparent 1px 3px);
    box-shadow: inset 0 0 0 1px var(--seam-bright);
  }
  .modsmore {
    margin: var(--space-2) 0 0 2px;
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

  /* A finger needs more than a pointer does. */
  @media (pointer: coarse) {
    .act {
      min-height: 44px;
    }
    .linkbtn:not(.grow) {
      width: 44px;
    }
    .disclose,
    .more {
      min-height: 44px;
    }
    .more {
      display: flex;
      align-items: center;
    }
    .mrow {
      min-height: 40px;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .disclose.busy :global(.glyph) {
      animation: none;
    }
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
    .desc,
    .talk {
      padding: var(--space-3) var(--space-4);
    }
    .modsec {
      padding: var(--space-3);
    }
    .mods {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
