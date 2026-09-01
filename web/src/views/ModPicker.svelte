<script lang="ts">
  import { Dialog } from 'bits-ui';
  import { api, ApiError } from '../lib/api';
  import { t } from '../lib/i18n.svelte';
  import type { ModHit, ModrinthVersion, SourceDecl, VersionRow } from '../lib/types';
  import ModIcon from './ModIcon.svelte';
  import { settle, stagger } from '../lib/motion.svelte';

  // Finding a mod, over both places one can come from. Which of the two holds it
  // is the mirror's problem, not a door to pick before the question (#101): the
  // search answers from the registry and Modrinth at once, and the row says what
  // it found. Copying a whole build's mod set, or picking a raw jar by hash, are
  // different questions and stay in MirrorPicker.

  let {
    packId,
    mc,
    loader,
    present = [],
    initialQuery,
    onPick,
    onClose,
  }: {
    packId: string;
    mc?: string;
    loader?: string;
    /// source keys the pack already declares -- shown, but not offered again
    present?: string[];
    initialQuery?: string;
    onPick: (sel: { filename: string; source: SourceDecl }) => void;
    onClose: () => void;
  } = $props();

  const presentSet = $derived(new Set(present));

  // svelte-ignore state_referenced_locally -- a mount-time prefill by design
  let q = $state(initialQuery ?? '');
  let hits = $state<ModHit[]>([]);
  let busy = $state(false);
  let err = $state('');
  let timer: ReturnType<typeof setTimeout> | undefined;

  // step two: which build of the chosen mod
  let sel = $state<ModHit | null>(null);
  let mirrorVersions = $state<VersionRow[]>([]);
  let upstreamVersions = $state<ModrinthVersion[]>([]);
  let versionsBusy = $state(false);

  // Which search is the current one. The debounce narrows the window; it does
  // not close it, and a slow first request landing after a narrower second
  // leaves rows on screen that do not answer what is typed above them.
  let generation = 0;

  async function search() {
    const query = q.trim();
    if (!query) {
      hits = [];
      return;
    }
    const mine = ++generation;
    busy = true;
    err = '';
    try {
      const found = await api.searchMods(query, { mc, loader, pack: packId });
      if (mine !== generation) return;
      hits = found;
    } catch (e) {
      if (mine !== generation) return;
      err = e instanceof ApiError ? `${e.status} ${e.body}` : String(e);
    } finally {
      if (mine === generation) busy = false;
    }
  }
  function onInput() {
    clearTimeout(timer);
    timer = setTimeout(search, 300);
  }
  // svelte-ignore state_referenced_locally -- fires once for the prefill
  if (q.trim()) void search();

  // A hit is already in the pack when its identity is: the Modrinth project,
  // or -- for a mirror-only mod -- the newest artifact the mirror holds for it,
  // which is the sha1 the hit carries. Version is deliberately not part of the
  // Modrinth half: another build of a mod the pack ships is still that mod.
  //
  // The hash half is one artifact rather than all of them, because that is what
  // a search hit carries. So a pack shipping an older build of a mirror-only mod
  // is not recognised and the mod is offered again. Erring that way on purpose:
  // a false "already in the pack" disables the button on a mod somebody wants,
  // which is worse than a duplicate row the editor already refuses on save.
  const inPack = (h: ModHit) =>
    (!!h.modrinth_project_id && presentSet.has(`m:${h.modrinth_project_id}`)) ||
    (!!h.icon_sha1 && presentSet.has(`c:${h.icon_sha1}`));

  async function open(h: ModHit) {
    sel = h;
    mirrorVersions = [];
    upstreamVersions = [];
    err = '';
    versionsBusy = true;
    try {
      // The mirror's own record is the better answer where it exists: it knows
      // each artifact's hash, whether the bytes are here, and its Modrinth pin.
      if (h.mod_id != null) mirrorVersions = await api.registryModVersions(h.mod_id);
      else if (h.modrinth_project_id)
        upstreamVersions = await api.modrinthVersions(h.modrinth_project_id, mc);
    } catch (e) {
      err = e instanceof ApiError ? `${e.status} ${e.body}` : String(e);
    } finally {
      versionsBusy = false;
    }
  }

  function back() {
    sel = null;
    mirrorVersions = [];
    upstreamVersions = [];
    err = '';
  }

  // A mirrored artifact is declared by hash; one the mirror knows but does not
  // hold is declared as the Modrinth pin it came from. Neither is a choice the
  // curator should have to make.
  function sourceFor(v: VersionRow): SourceDecl | null {
    if (v.cached) return { type: 'smrt_cache', sha1: v.sha1 };
    if (v.modrinth_project_id && v.modrinth_version_id)
      return {
        type: 'modrinth',
        project_id: v.modrinth_project_id,
        version_id: v.modrinth_version_id,
      };
    return null;
  }

  function pickMirror(v: VersionRow) {
    const source = sourceFor(v);
    if (!source) return;
    onPick({ filename: v.filename || `${sel?.slug ?? v.sha1.slice(0, 12)}.jar`, source });
  }

  // Upstream sometimes publishes a version whose jar never landed: the metadata
  // lists an empty file array. Such a pin resolves to nothing at build time.
  const hasFile = (v: ModrinthVersion) => (v.files?.length ?? 0) > 0;

  function pickUpstream(v: ModrinthVersion) {
    if (!sel?.modrinth_project_id || !hasFile(v)) return;
    onPick({
      filename: v.files[0]?.filename || `${sel.slug ?? sel.name}.jar`,
      source: { type: 'modrinth', project_id: sel.modrinth_project_id, version_id: v.id },
    });
  }

  // ModIcon resolves an icon from a declared source; a search hit is not a
  // declaration yet, so it is handed the identity it does have -- the Modrinth
  // project, or the artifact hash the mirror knows it by.
  function iconSource(h: ModHit): SourceDecl {
    if (h.modrinth_project_id)
      return { type: 'modrinth', project_id: h.modrinth_project_id, version_id: '' };
    if (h.icon_sha1) return { type: 'smrt_cache', sha1: h.icon_sha1 };
    return { type: 'smrt_static', rel_path: '' };
  }

  // What the loader verdict says in the row. Native says nothing: it is the
  // expected case, and a badge on every line is noise.
  function fitLabel(h: ModHit): string | null {
    if (h.fit === 'bridged') return t('mp.fit.bridged', { loader: h.bridged_from ?? '' });
    if (h.fit === 'bridgeable') return t('mp.fit.bridgeable', { loader: h.bridged_from ?? '' });
    if (h.fit === 'foreign') return t('mp.fit.foreign', { loaders: h.loaders.join(', ') });
    return null;
  }
  const fitKind = (h: ModHit) =>
    h.fit === 'bridged' ? 'ok' : h.fit === 'bridgeable' ? 'warn' : 'danger';
</script>

<Dialog.Root open onOpenChange={(o) => !o && onClose()}>
  <Dialog.Portal>
    <Dialog.Overlay class="dlg-scrim" />
    <Dialog.Content class="picker panel">
      <div class="hd">
        <Dialog.Title class="ttl">{sel ? sel.name : t('mp.title')}</Dialog.Title>
        {#if sel}<button class="sm" onclick={back}>{t('mp.back')}</button>{/if}
        <div class="spacer"></div>
        <button class="sm" onclick={onClose}>{t('common.close')}</button>
      </div>

      {#if !sel}
        <input
          class="q"
          bind:value={q}
          oninput={onInput}
          placeholder={t('mp.placeholder')}
          aria-label={t('mp.title')}
        />
        <div class="hint faint">{t('mp.hint')}</div>
      {/if}

      {#if err}<div class="err mono">{err}</div>{/if}

      <div class="list scroll">
        {#if !sel}
          {#if busy && hits.length === 0}<div class="muted s">{t('common.loading')}</div>{/if}
          {#each hits as h, i (h.modrinth_project_id ?? h.mod_id ?? h.name)}
            {@const label = fitLabel(h)}
            <!-- Results arrive in the order they are read, and a hit that
                 survives a narrowing query slides to its new place rather than
                 teleporting: typing one more letter should look like the list
                 settling, not like a different list. -->
            <button class="hit row-in" use:stagger={i} animate:settle disabled={inPack(h)} onclick={() => open(h)}>
              <ModIcon
                name={h.name}
                iconUrl={h.icon_url ?? null}
                source={iconSource(h)}
                sha1={h.icon_sha1 ?? null}
                size={28}
                mono
              />
              <div class="info">
                <div class="t">
                  {h.name}
                  {#if h.slug}<span class="faint mono">{h.slug}</span>{/if}
                  {#if h.mirrored}<span class="chip ok">{t('mp.mirrored')}</span>{/if}
                  {#if label}<span class="chip {fitKind(h)}">{label}</span>{/if}
                  {#if inPack(h)}<span class="chip muted">{t('mp.inPack')}</span>{/if}
                </div>
                {#if h.description}<div class="d muted">{h.description}</div>{/if}
              </div>
            </button>
          {/each}
          {#if !busy && q.trim() && hits.length === 0}
            <div class="muted s">{t('mp.noResults')}</div>
          {/if}
        {:else}
          {#if versionsBusy}<div class="muted s">{t('common.loading')}</div>{/if}
          {#each mirrorVersions as v (v.sha1)}
            {@const source = sourceFor(v)}
            <button class="ver" disabled={!source} onclick={() => pickMirror(v)}>
              <span class="vn mono">{v.version}</span>
              <span class="faint mono">{v.targets.join(', ')}</span>
              <span class="spacer"></span>
              {#if v.cached}<span class="chip ok">{t('mp.fromMirror')}</span>
              {:else if source}<span class="chip">{t('mp.fromModrinth')}</span>
              {:else}<span class="chip muted">{t('mp.noSource')}</span>{/if}
            </button>
          {/each}
          {#each upstreamVersions as v (v.id)}
            <button class="ver" disabled={!hasFile(v)} onclick={() => pickUpstream(v)}>
              <span class="vn mono">{v.version_number}</span>
              <span class="faint mono">{v.loaders.join(', ')}</span>
              <span class="spacer"></span>
              {#if !hasFile(v)}<span class="chip muted">{t('mp.noJar')}</span>
              {:else}<span class="chip">{v.version_type ?? ''}</span>{/if}
            </button>
          {/each}
          {#if !versionsBusy && mirrorVersions.length === 0 && upstreamVersions.length === 0}
            <div class="muted s">{t('mp.noVersions')}</div>
          {/if}
        {/if}
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<style>
  :global(.picker) {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    z-index: 61;
    width: 720px;
    max-width: 94vw;
    max-height: 82vh;
    display: flex;
    flex-direction: column;
    padding: var(--space-4);
    gap: var(--space-3);
  }
  .hd {
    display: flex;
    align-items: center;
    gap: var(--space-3);
  }
  .spacer {
    flex: 1;
  }
  .q {
    width: 100%;
  }
  .hint {
    font-size: var(--fs-xs);
    margin-top: -4px;
  }
  .list {
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    min-height: 120px;
  }
  .hit,
  .ver {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    width: 100%;
    text-align: left;
    padding: var(--space-2) var(--space-3);
  }
  .hit:disabled,
  .ver:disabled {
    opacity: 0.55;
  }
  .info {
    min-width: 0;
  }
  .t {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-wrap: wrap;
    font-size: var(--fs-md);
  }
  .d {
    font-size: var(--fs-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .vn {
    font-size: var(--fs-sm);
  }
  .err {
    color: var(--danger);
    font-size: var(--fs-sm);
  }
  .s {
    padding: var(--space-3);
    font-size: var(--fs-sm);
  }
  @container view (max-width: 560px) {
    :global(.picker) {
      width: 96vw;
    }
  }
</style>
