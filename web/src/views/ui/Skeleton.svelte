<script lang="ts">
  import { t } from '../../lib/i18n.svelte';

  // A wait that has the shape of what is coming.
  //
  // Held back for a moment first: an answer that arrives in eighty milliseconds
  // does not need a placeholder, and showing one anyway is a flash of grey that
  // reads as a stutter rather than as speed. Past the threshold the wait is long
  // enough to be worth acknowledging, and the blocks hold the layout so the rows
  // land where the placeholders were.
  //
  // `row` draws a row's interior -- a leading square and two lines of text --
  // rather than a flat slab, because a slab says only that something is coming
  // and a list of packs is a specific something. `bar` is the slab, for the
  // waits that really are undifferentiated text.
  let {
    rows = 3,
    height = 46,
    gap = 8,
    shape = 'bar',
    lead = 34,
    delay = 250,
  }: {
    rows?: number;
    /// Row height in px -- match what is coming, or the layout still jumps.
    height?: number;
    /// Space between blocks. Zero for a list whose rows are separated by a rule
    /// rather than by a gap; the rule is then drawn here too.
    gap?: number;
    shape?: 'bar' | 'row';
    /// Size of the leading square on a `row`, in px. Zero for a list whose rows
    /// carry no icon.
    lead?: number;
    /// How long a wait has to be before it is worth showing at all.
    delay?: number;
  } = $props();

  // Rows of one width read as a barcode; content does not line up like that.
  // Cycled rather than random so a re-render does not reshuffle the placeholder
  // somebody is already looking at.
  const TITLE = ['42%', '31%', '52%', '37%', '46%'];
  const SUB = ['64%', '73%', '58%', '69%', '61%'];

  let show = $state(false);
  // The blocks are the visual half of the wait and the only half a screen reader
  // gets nothing from; this carries the other half. It is not held behind the
  // delay -- that exists to stop a flash of grey, and there is no such thing as
  // a flash of speech. Said one tick after mount so the live region is in the
  // page before it has anything to announce, which is what makes it announce.
  let said = $state(false);
  $effect(() => {
    const blocks = setTimeout(() => (show = true), delay);
    const speech = setTimeout(() => (said = true), 0);
    return () => {
      clearTimeout(blocks);
      clearTimeout(speech);
    };
  });
</script>

<span class="vh" role="status">{said ? t('common.loading') : ''}</span>
{#if show}
  <!-- aria-hidden: the region above says the list is busy, and reading out a row
       of empty boxes tells nobody anything. -->
  <div class="skel" class:seamed={gap === 0} style="gap:{gap}px" aria-hidden="true">
    {#each { length: rows } as _, i (i)}
      {#if shape === 'row'}
        <div class="line" style="height:{height}px">
          {#if lead > 0}
            <div class="sk lead" style="width:{lead}px;height:{lead}px"></div>
          {/if}
          <div class="text">
            <div class="sk ln" style="width:{TITLE[i % TITLE.length]};height:12px"></div>
            <div class="sk ln" style="width:{SUB[i % SUB.length]};height:9px"></div>
          </div>
        </div>
      {:else}
        <div class="sk" style="height:{height}px"></div>
      {/if}
    {/each}
  </div>
{/if}

<style>
  .skel {
    display: flex;
    flex-direction: column;
  }
  /* A list whose rows are divided by a rule rather than by a gap keeps its
     divisions while it waits; without them the placeholders fuse into one slab
     and stop reading as rows at all. */
  .skel.seamed > * + * {
    border-top: 1px solid var(--seam);
  }
  .line {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: 0 var(--space-3);
  }
  .lead {
    flex: none;
  }
  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
  .ln {
    border-radius: 3px;
  }
</style>
