<script lang="ts">
  import { letterAvatar, readableInitials } from '../lib/preview';
  import type { Source, SourceDecl } from '../lib/types';

  // accepts the wire Source (preview) or the authoring SourceDecl (editor) -- both
  // carry `type`, and the modrinth variant of each carries `project_id`, which is
  // all this component reads
  let {
    name,
    iconUrl = null,
    source,
    sha1 = null,
    size = 34,
    mono = false,
  }: {
    name: string;
    iconUrl?: string | null;
    source: Source | SourceDecl;
    // the artifact's sha1 where the caller knows it (manifest entries, registry
    // rows): unlocks the jar-embedded icon fallback for Modrinth-sourced mods
    // whose bytes the mirror caches anyway
    sha1?: string | null;
    size?: number;
    // monochrome letter fallback for the control panel; the launcher preview
    // keeps the hashed colour
    mono?: boolean;
  } = $props();

  const avatar = $derived(letterAvatar(name));
  const initials = $derived(mono ? readableInitials(name) : avatar.initials);
  const explicit = $derived(iconUrl?.trim() || null);
  // index into the candidate chain; a load error advances to the next candidate
  // instead of giving up, so a project the mirror has no icon for degrades to
  // the jar's own embedded icon before the letter avatar
  let failed = $state(0);
  const cacheSha = $derived(
    source.type === 'smrt_cache' && 'sha1' in source && source.sha1 ? source.sha1 : sha1,
  );
  // Both icon candidates are addresses on this mirror, so drawing a row costs
  // no request of its own: the browser fetches the picture, once, and caches
  // it. Asking the mirror for a Modrinth url and then loading it off Modrinth's
  // CDN cost a round trip per row and told a third party who was looking.
  const projectIcon = $derived(
    source.type === 'modrinth' && source.project_id
      ? `/v1/modrinth/icon/${encodeURIComponent(source.project_id)}`
      : null,
  );
  const candidates = $derived(
    [explicit, projectIcon, cacheSha ? `/v1/cache/icon/${cacheSha}` : null].filter(
      (c): c is string => !!c,
    ),
  );
  const src = $derived(candidates[failed] ?? null);

  // A list row may be reused for a different mod (sort / re-point), so the
  // walk down the candidates restarts with the mod rather than carrying the
  // previous one's failures over.
  $effect(() => {
    void source;
    failed = 0;
  });
</script>

{#if src}
  <img
    class="mi"
    style="width:{size}px;height:{size}px"
    {src}
    alt={name}
    loading="lazy"
    onerror={() => (failed = failed + 1)}
  />
{:else}
  <span
    class="mi avatar"
    class:mono
    style="width:{size}px;height:{size}px;font-size:{Math.round(size * 0.4)}px{mono
      ? ''
      : `;background:${avatar.color}`}"
    aria-label={name}>{initials}</span
  >
{/if}

<style>
  .mi {
    flex: none;
    border-radius: 6px;
    object-fit: cover;
  }
  .avatar {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: #fff;
    font-weight: 600;
    letter-spacing: -0.02em;
    user-select: none;
  }
  .avatar.mono {
    background: var(--panel-3);
    color: var(--fg-dim);
  }
</style>
