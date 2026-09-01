<script lang="ts">
  import { api, ApiError } from '../lib/api';
  import { notifyFail } from '../lib/toasts.svelte';
  import { dialogs } from '../lib/dialogs.svelte';
  import { href, plainClick, route } from '../lib/route.svelte';
  import { t } from '../lib/i18n.svelte';
  import { mirror } from '../lib/mirror.svelte';
  import { terms } from '../lib/terms.svelte';
  import { renderMarkdown } from '../lib/markdown';
  import ModIcon from './ModIcon.svelte';
  import PackThreads from './PackThreads.svelte';
  import TabStrip from './ui/TabStrip.svelte';
  import Skeleton from './ui/Skeleton.svelte';
  import type { CommunityPack, PackManifest, PackSummary } from '../lib/types';

  // A signed-in member can fork any pack they can browse into their namespace.
  let {
    me,
    onSignIn,
  }: { me: { uid: number; login: string } | null; onSignIn?: () => void } = $props();

  // Guest-facing, read-only. Official packs are the launcher contract (/v1/packs);
  // community packs (/v1/community) are site-only, browseable but not in the
  // launcher's catalog. Detail reads /v1/packs/:id/manifest for both.
  type Tab = 'official' | 'community';
  let tab = $state<Tab>('official');
  const tabTabs = $derived([
    { value: 'official', label: t('browse.official') },
    { value: 'community', label: t('browse.community') },
  ]);
  let packs = $state<PackSummary[]>([]);
  let community = $state<CommunityPack[]>([]);
  let loading = $state(true);

  let openId = $state<string | null>(null);
  let manifest = $state<PackManifest | null>(null);
  let mLoading = $state(false);

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
  load();

  // unified rows: official packs have no byline, community packs show `by <user>`
  const items = $derived<{ summary: PackSummary; owner: string | null }[]>(
    tab === 'official'
      ? packs.map((p) => ({ summary: p, owner: null }))
      : community.map((c) => ({ summary: c.summary, owner: c.owner_login })),
  );

  async function open(p: PackSummary) {
    if (openId === p.pack_id) {
      openId = null;
      manifest = null;
      return;
    }
    openId = p.pack_id;
    manifest = null;
    mLoading = true;
    try {
      manifest = await api.manifest(p.pack_id);
    } catch (e) {
      notifyFail(e);
    } finally {
      mLoading = false;
    }
  }

  function modName(m: PackManifest['mods'][number]): string {
    return m.display?.name ?? m.filename.replace(/\.jar$/, '');
  }

  async function fork(p: PackSummary) {
    if (!me) return;
    if (!(await terms.ensure())) return;
    const name = (
      await dialogs.prompt(t('browse.forkPrompt'), {
        title: t('browse.fork'),
        initial: p.pack_id.split('/').pop() ?? p.pack_id,
      })
    )?.trim();
    if (!name) return;
    try {
      await api.fork(p.pack_id, name);
      await dialogs.confirm(t('browse.forked', { name }), { title: t('browse.fork') });
    } catch (e) {
      notifyFail(e);
    }
  }

  // a publish changes the catalog; the featured selection changes what leads it
  $effect(() => {
    if (mirror.packs + mirror.catalog > 0) load();
  });
</script>

<div class="view">

  <TabStrip variant="pill" value={tab} tabs={tabTabs} onChange={(v) => (tab = v as Tab)} />

  <!-- The catalog is the first thing a guest sees, and the first read of it used
       to be a blank page: no rows yet, and the empty state deliberately held
       back until the answer was in. Holding the shape of the rows says the
       mirror is answering; saying nothing says it is bare. -->
  {#if items.length === 0 && loading}
    <!-- in the list's own frame, so the card does not appear around the rows a
         moment after the rows do -->
    <div class="panel plist"><Skeleton rows={5} height={63} gap={0} shape="row" lead={34} /></div>
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
  <div class="panel plist" class:stale={loading}>
    {#each items as { summary: p, owner } (p.pack_id)}
      <div class="pack" class:open={openId === p.pack_id}>
        <button class="prow" onclick={() => open(p)}>
          <span class="chev" aria-hidden="true">&#9656;</span>
          {#if p.icon_url}
            <img class="picon" src={p.icon_url} alt={p.display_name} loading="lazy" />
          {:else}
            <span class="picon avatar mono">{p.display_name.slice(0, 1).toUpperCase()}</span>
          {/if}
          <div class="pinfo">
            <div class="pname">
              {p.display_name}{#if p.featured}<span class="feat mono">{t('packs.flag.featured')}</span>{/if}
            </div>
            {#if owner}<div class="pby faint mono">{t('browse.by', { user: owner })}</div>{/if}
            {#if p.tagline}<div class="ptag muted">{p.tagline}</div>{/if}
          </div>
          <div class="pmeta">
            <span class="tag">{p.minecraft_version}</span>
            <span class="pver mono faint">{p.latest_pack_version}</span>
          </div>
        </button>

        {#if openId === p.pack_id}
          <div class="detail">
            {#if mLoading}
              <Skeleton rows={3} height={22} />
            {:else if manifest}
              {#if me}
                <button class="forkbtn" onclick={() => fork(p)}>{t('browse.fork')}</button>
              {/if}
              {#if p.description_md}
                <!-- renderMarkdown sanitizes; safe to inject -->
                <div class="desc">{@html renderMarkdown(p.description_md)}</div>
              {/if}
              <!-- What is being asked of this pack, where the pack is. A
                   decision nobody can find is indistinguishable from one nobody
                   made, so the discussion is part of the pack's public face
                   rather than something only its keepers ever see. -->
              <details class="talk">
                <summary>{t('browse.discussion')}</summary>
                <PackThreads packId={p.pack_id} standalone canWrite={me != null} />
                {#if !me}
                  <p class="muted signin">{t('browse.signInToReport')}</p>
                {/if}
              </details>
              <div class="modhead mono faint">{t('browse.modsN', { n: manifest.mods.length })}</div>
              <div class="mods">
                {#each manifest.mods as m (m.sha1)}
                  <a
                    class="mrow"
                    href={href.mod(`sha1:${m.sha1}`)}
                    onclick={(e) => {
                      if (!plainClick(e)) return;
                      e.preventDefault();
                      route.openMod(`sha1:${m.sha1}`);
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
                    {#if !m.required}<span class="opt mono">{t('browse.optional')}</span>{/if}
                  </a>
                {/each}
              </div>
            {/if}
          </div>
        {/if}
      </div>
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
  .plist {
    overflow: hidden;
  }
  .pby {
    font-size: var(--fs-xs);
    margin-top: 2px;
  }
  .forkbtn {
    align-self: flex-start;
    padding: 5px 14px;
    font-size: var(--fs-sm);
  }
  .pack {
    border-bottom: 1px solid var(--seam);
  }
  .pack:last-child {
    border-bottom: none;
  }
  .prow {
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
  .prow:hover {
    background: var(--panel-2);
  }
  .chev {
    color: var(--fg-faint);
    font-size: var(--fs-xs);
    flex: none;
    transition: transform var(--dur-state) var(--ease-out);
  }
  .pack.open .chev {
    transform: rotate(90deg);
    color: var(--fg-dim);
  }
  .picon {
    width: 34px;
    height: 34px;
    flex: none;
    border-radius: var(--radius-sm);
    object-fit: cover;
  }
  .avatar {
    display: grid;
    place-items: center;
    background: var(--panel-3);
    color: var(--fg-dim);
    font-size: var(--fs-lg);
  }
  .pinfo {
    flex: 1;
    min-width: 0;
  }
  .pname {
    font-size: var(--fs-lg);
    font-weight: 600;
  }
  .feat {
    margin-left: 8px;
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--accent);
    font-weight: 400;
  }
  .ptag {
    font-size: var(--fs-sm);
    margin-top: 2px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pmeta {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    flex-shrink: 0;
  }
  .pver {
    font-size: var(--fs-xs);
  }
  .detail {
    padding: 2px var(--space-3) var(--space-4) 42px;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .desc {
    font-size: var(--fs-md);
    line-height: 1.55;
    color: var(--fg-dim);
    max-width: 65ch;
  }
  .modhead {
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }
  /* Folded away by default: the mod list is what a reader came for, and an
     empty discussion should not push it down the page. */
  .talk {
    border: 1px solid var(--seam);
    border-radius: var(--radius-sm);
    padding: var(--space-2) var(--space-3);
    max-width: 720px;
  }
  .talk summary {
    cursor: pointer;
    font-size: var(--fs-sm);
    color: var(--fg-dim);
  }
  .talk[open] summary {
    margin-bottom: var(--space-2);
  }
  .signin {
    font-size: var(--fs-sm);
    margin: 0;
  }
  .mods {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: 6px var(--space-4);
  }
  .mrow {
    display: flex;
    color: inherit;
    text-decoration: none;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    width: 100%;
    text-align: left;
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    padding: 3px 6px;
    margin: 0 -6px;
    cursor: pointer;
    color: inherit;
  }
  .mrow:hover {
    background: var(--panel-2);
  }
  .mrow:hover .mn {
    text-decoration: underline;
    text-decoration-color: var(--seam-bright);
    text-underline-offset: 2px;
  }
  .mn {
    font-size: var(--fs-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .opt {
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-faint);
    flex-shrink: 0;
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
