<script lang="ts">
  import { api } from '../lib/api';
  import { notifyFail } from '../lib/toasts.svelte';
  import { href, plainClick, route } from '../lib/route.svelte';
  import { i18n, t } from '../lib/i18n.svelte';
  import { mirror } from '../lib/mirror.svelte';
  import { inLanguage } from '../lib/cardtext';
  import TabStrip from './ui/TabStrip.svelte';
  import type { CommunityPack, PackSummary } from '../lib/types';

  let {
    me,
    onSignIn,
  }: { me: { uid: number; login: string } | null; onSignIn?: () => void } = $props();

  // The catalog is the showcase: a card per pack, led by the pack's banner, and
  // each card opens the pack's own page. Official packs are the launcher
  // contract (/v1/packs). Community packs (/v1/community) are site-only,
  // browseable here but not in the launcher's catalog.
  type Tab = 'official' | 'community';
  let tab = $state<Tab>('official');
  const tabTabs = $derived([
    { value: 'official', label: t('browse.official') },
    { value: 'community', label: t('browse.community') },
  ]);
  let packs = $state<PackSummary[]>([]);
  let community = $state<CommunityPack[]>([]);
  let loading = $state(true);
  // a banner that failed to load falls back to the plain cover rather than the
  // browser's broken-image glyph
  let broken = $state<Record<string, boolean>>({});

  async function load() {
    loading = true;
    try {
      const [p, c] = await Promise.all([api.packs(), api.community()]);
      packs = p.packs;
      community = c;
    } catch (e) {
      notifyFail(e);
    } finally {
      loading = false;
    }
  }

  // Loaded on arrival and again when a publish changes the catalog or the
  // featured selection changes what leads it.
  $effect(() => {
    void (mirror.packs + mirror.catalog);
    void load();
  });

  // official packs carry no byline, community packs say whose they are
  const items = $derived<{ summary: PackSummary; owner: string | null }[]>(
    tab === 'official'
      ? packs.map((p) => ({ summary: p, owner: null }))
      : community.map((c) => ({ summary: c.summary, owner: c.owner_login })),
  );

  // The reader's language where the pack was written in it, the untagged copy
  // otherwise. A function rather than a derived value because the catalog
  // renders a list of them.
  const tagline = (p: PackSummary) => inLanguage(p.tagline, p.tagline_i18n, i18n.locale);
</script>

<div class="view">
  <TabStrip variant="pill" value={tab} tabs={tabTabs} onChange={(v) => (tab = v as Tab)} />

  <!-- The catalog is the first thing a guest sees. Holding the shape of the
       cards says the mirror is answering, where a blank page says it is bare. -->
  {#if items.length === 0 && loading}
    <div class="grid" aria-hidden="true">
      {#each Array(6) as _, i (i)}<div class="card skcard"><div class="sk skcover"></div><div class="sk skline"></div><div class="sk skline short"></div></div>{/each}
    </div>
  {:else if items.length === 0}
    <div class="emptystate">
      <span class="mk" aria-hidden="true"></span>
      <div class="etitle">
        {tab === 'community' ? t('browse.emptyCommunity') : t('browse.empty')}
      </div>
      {#if me}
        <p class="esub muted">{t('browse.emptySubMember')}</p>
      {:else}
        <p class="esub muted">{t('browse.emptySubGuest')}</p>
        {#if onSignIn}
          <button class="primary" onclick={onSignIn}>{t('shell.signIn')}</button>
        {/if}
      {/if}
    </div>
  {:else}
    <div class="grid" class:stale={loading}>
      {#each items as { summary: p, owner } (p.pack_id)}
        <a
          class="card"
          href={href.catalog(p.pack_id)}
          onclick={(e) => {
            if (!plainClick(e)) return;
            e.preventDefault();
            route.openCatalog(p.pack_id);
          }}
        >
          <div class="cover">
            {#if p.banner_url && !broken[p.pack_id]}
              <img
                src={p.banner_url}
                alt=""
                loading="lazy"
                onerror={() => (broken = { ...broken, [p.pack_id]: true })}
              />
            {:else}
              <span class="ph mono" aria-hidden="true">{p.display_name.slice(0, 1).toUpperCase()}</span>
            {/if}
            {#if p.icon_url}<img class="cicon" src={p.icon_url} alt="" loading="lazy" />{/if}
          </div>
          <div class="cbody">
            <div class="cname">
              <span class="nm">{p.display_name}</span>
              {#if p.featured}<span class="feat mono">{t('packs.flag.featured')}</span>{/if}
            </div>
            {#if owner}<div class="cby faint mono">{t('browse.by', { user: owner })}</div>{/if}
            <p class="ctag muted">{tagline(p)}</p>
            <div class="cmeta">
              <span class="tag">{p.minecraft_version}</span>
              <span class="ver mono faint">{p.latest_pack_version}</span>
            </div>
          </div>
        </a>
      {/each}
    </div>
  {/if}
</div>

<style>
  .view {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: var(--space-4);
  }
  .card {
    display: flex;
    flex-direction: column;
    min-width: 0;
    overflow: hidden;
    color: inherit;
    text-decoration: none;
    background: var(--panel);
    border: 1px solid var(--seam);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-1);
    transition: border-color var(--dur-state) var(--ease-out);
  }
  .card:hover {
    border-color: var(--seam-bright);
  }
  .card:hover .nm {
    text-decoration: underline;
    text-decoration-color: var(--seam-bright);
    text-underline-offset: 3px;
  }
  .card:focus-visible {
    outline: none;
    box-shadow: var(--ring);
  }
  /* Every card has a cover, banner or not, so the grid keeps one rhythm and a
     pack without a picture does not read as a broken one. A banner is wider
     than the card, and a server banner paints its name into the left corner,
     so the crop keeps the left edge and lets the right one go. */
  .cover {
    position: relative;
    aspect-ratio: 3 / 1;
    background:
      linear-gradient(135deg, var(--accent-hue-soft), transparent 70%),
      var(--panel-2);
    border-bottom: 1px solid var(--seam);
  }
  .cover > img:not(.cicon) {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    object-position: left center;
  }
  .ph {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    font-size: 36px;
    color: var(--fg-faint);
  }
  .cicon {
    position: absolute;
    left: var(--space-3);
    bottom: calc(-1 * var(--space-4));
    width: 44px;
    height: 44px;
    border-radius: var(--radius-sm);
    object-fit: cover;
    border: 2px solid var(--panel);
    background: var(--panel);
  }
  .cbody {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: var(--space-3) var(--space-4) var(--space-4);
  }
  .cover:has(.cicon) + .cbody {
    padding-top: calc(var(--space-4) + var(--space-3));
  }
  .cname {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 4px 8px;
    font-size: var(--fs-lg);
    font-weight: 600;
    line-height: 1.3;
  }
  .nm {
    overflow-wrap: anywhere;
  }
  .feat {
    font-size: var(--fs-xs);
    font-weight: 400;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--accent-hue);
  }
  .cby {
    font-size: var(--fs-xs);
  }
  /* Two lines held whether or not there is a tagline, so a pack without one
     does not make its card the short one in the row. */
  .ctag {
    margin: 0;
    font-size: var(--fs-sm);
    line-height: 1.45;
    min-height: calc(2 * 1.45em);
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .cmeta {
    margin-top: auto;
    padding-top: var(--space-2);
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .ver {
    margin-left: auto;
    font-size: var(--fs-xs);
  }

  .skcard {
    gap: var(--space-2);
    padding-bottom: var(--space-4);
  }
  .skcover {
    aspect-ratio: 3 / 1;
    border-radius: 0;
  }
  .skline {
    height: 14px;
    margin: 0 var(--space-4);
  }
  .skline.short {
    width: 45%;
  }

  /* Empty catalog: a centred block with somewhere to go, not a thin bar of text
     stranded at the top of the void. */
  .emptystate {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    text-align: center;
    padding: clamp(48px, 12vh, 120px) var(--space-4);
  }
  .emptystate .mk {
    width: 34px;
    height: 34px;
    border-radius: 9px;
    background: var(--panel-3);
    border: 1px solid var(--seam);
    margin-bottom: var(--space-1);
  }
  .etitle {
    font-size: var(--fs-lg);
    font-weight: 600;
  }
  .esub {
    margin: 0;
    max-width: 44ch;
    font-size: var(--fs-md);
    line-height: 1.55;
  }
  .emptystate .primary {
    margin-top: var(--space-2);
  }
</style>
