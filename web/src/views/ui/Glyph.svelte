<script lang="ts" module>
  export type GlyphName =
    | 'fork'
    | 'edit'
    | 'link'
    | 'check'
    | 'chat'
    | 'chevron'
    | 'clock'
    | 'cube'
    | 'loader'
    | 'java'
    | 'download'
    | 'notes'
    | 'search'
    | 'alert'
    | 'gear'
    | 'user'
    | 'exit'
    | 'enter';
</script>

<script lang="ts">
  // Small action and fact glyphs, drawn on the same 16px grid and stroke as
  // the rail's own icons so the two sets read as one. Inline, in currentColor,
  // so a glyph takes the colour of the control or line it sits in.
  let { name, size = 16 }: { name: GlyphName; size?: number } = $props();
</script>

<svg
  class="glyph"
  width={size}
  height={size}
  viewBox="0 0 16 16"
  fill="none"
  stroke="currentColor"
  stroke-width="1.3"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  {#if name === 'fork'}
    <circle cx="4.5" cy="3.4" r="1.6" />
    <circle cx="11.5" cy="3.4" r="1.6" />
    <circle cx="8" cy="12.6" r="1.6" />
    <path d="M4.5 5v1.4c0 1.4 1 2.3 2.4 2.3h2.2c1.4 0 2.4-0.9 2.4-2.3V5" />
    <path d="M8 8.7V11" />
  {:else if name === 'edit'}
    <path d="M10.6 2.6l2.8 2.8-7.6 7.6-3.3 0.5 0.5-3.3z" />
    <path d="M9.2 4l2.8 2.8" />
  {:else if name === 'link'}
    <path d="M6.6 9.4l2.8-2.8" />
    <path d="M7.4 4.6l1.2-1.2a2.6 2.6 0 0 1 3.7 3.7l-1.2 1.2" />
    <path d="M8.6 11.4l-1.2 1.2a2.6 2.6 0 0 1-3.7-3.7l1.2-1.2" />
  {:else if name === 'check'}
    <path d="M3.2 8.4l3 3 6.6-6.8" />
  {:else if name === 'gear'}
    <circle cx="8" cy="8" r="2.1" />
    <path d="M8 1.9v1.6M8 12.5v1.6M1.9 8h1.6M12.5 8h1.6M3.7 3.7l1.1 1.1M11.2 11.2l1.1 1.1M3.7 12.3l1.1-1.1M11.2 4.8l1.1-1.1" />
  {:else if name === 'user'}
    <circle cx="8" cy="5.6" r="2.7" />
    <path d="M2.9 13.6c0.7-2.4 2.7-3.8 5.1-3.8s4.4 1.4 5.1 3.8" />
  {:else if name === 'exit'}
    <path d="M6.4 2.6H3.6c-0.6 0-1 0.4-1 1v8.8c0 0.6 0.4 1 1 1h2.8" />
    <path d="M10.4 5.2L13.2 8l-2.8 2.8" />
    <path d="M13.2 8H6.2" />
  {:else if name === 'enter'}
    <path d="M9.6 2.6h2.8c0.6 0 1 0.4 1 1v8.8c0 0.6-0.4 1-1 1H9.6" />
    <path d="M5.8 5.2L8.6 8l-2.8 2.8" />
    <path d="M8.6 8H1.8" />
  {:else if name === 'alert'}
    <path d="M8 2.4l5.9 10.3a0.9 0.9 0 0 1-0.8 1.3H2.9a0.9 0.9 0 0 1-0.8-1.3z" />
    <path d="M8 6.4v3.2" />
    <path d="M8 11.8v0.1" />
  {:else if name === 'chat'}
    <path d="M2.6 4.2c0-0.9 0.7-1.6 1.6-1.6h7.6c0.9 0 1.6 0.7 1.6 1.6v5.2c0 0.9-0.7 1.6-1.6 1.6H7.2L4.4 13.2V11h-0.2c-0.9 0-1.6-0.7-1.6-1.6z" />
  {:else if name === 'chevron'}
    <path d="M6 3.6l4.4 4.4L6 12.4" />
  {:else if name === 'clock'}
    <circle cx="8" cy="8" r="5.6" />
    <path d="M8 4.8V8l2.2 1.4" />
  {:else if name === 'cube'}
    <path d="M8 2.2l5.2 2.9v5.8L8 13.8l-5.2-2.9V5.1z" />
    <path d="M2.8 5.1L8 8l5.2-2.9" />
    <path d="M8 8v5.8" />
  {:else if name === 'loader'}
    <path d="M3 12.8h10" />
    <path d="M4.4 12.8l1-3.4h5.2l1 3.4" />
    <path d="M5.4 9.4V7.2h5.2v2.2" />
    <path d="M3.6 4.8c1.4-1.8 3-2.6 4.4-2.6s3 0.8 4.4 2.6" />
  {:else if name === 'java'}
    <path d="M3.4 6.6h7.8v3.2a3.4 3.4 0 0 1-3.4 3.4H6.8a3.4 3.4 0 0 1-3.4-3.4z" />
    <path d="M11.2 7.4h0.8a1.6 1.6 0 0 1 0 3.2h-0.9" />
    <path d="M6 2.4c-0.6 0.8 0.6 1.4 0 2.4" />
    <path d="M8.6 2.4c-0.6 0.8 0.6 1.4 0 2.4" />
  {:else if name === 'download'}
    <path d="M8 2.6v7.2" />
    <path d="M5 7l3 3 3-3" />
    <path d="M3 12.8h10" />
  {:else if name === 'notes'}
    <path d="M4 2.4h6l2.4 2.4v8.8H4z" />
    <path d="M6 7h4.4M6 9.4h4.4M6 11.6h2.6" />
  {:else if name === 'search'}
    <circle cx="7" cy="7" r="4.2" />
    <path d="M10.2 10.2l3.2 3.2" />
  {/if}
</svg>

<style>
  .glyph {
    flex: none;
    display: block;
  }
</style>
