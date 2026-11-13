<script lang="ts">
  // A pack in a list, by its picture. The banner where the pack has one, cropped
  // from the left because a server banner paints its name there, the icon where
  // it has only that, and its initials otherwise. One width for all three, so a
  // list of packs keeps its column whichever each one has.
  let {
    name,
    banner = null,
    icon = null,
  }: { name: string; banner?: string | null; icon?: string | null } = $props();

  // every address that failed to load, so a broken banner falls to the icon
  // and a broken icon to the initials rather than back to the banner
  let failed = $state<string[]>([]);
  const src = $derived(
    [banner, icon].find((u): u is string => !!u && !failed.includes(u)) ?? null,
  );
</script>

{#if src}
  <img
    class="thumb"
    class:icon={src === icon}
    {src}
    alt=""
    loading="lazy"
    onerror={() => (failed = [...failed, src])}
  />
{:else}
  <span class="thumb letters mono" aria-hidden="true">{name.slice(0, 2).toUpperCase()}</span>
{/if}

<style>
  .thumb {
    width: 56px;
    height: 32px;
    flex: none;
    border-radius: var(--radius-sm);
    object-fit: cover;
    object-position: left center;
    background: var(--panel-3);
  }
  .thumb.icon {
    object-fit: contain;
    object-position: center;
  }
  .letters {
    display: grid;
    place-items: center;
    font-size: var(--fs-xs);
    font-weight: 700;
    color: var(--fg-dim);
  }
</style>
