<script lang="ts">
  import ModIcon from './ModIcon.svelte';
  import { t } from '../lib/i18n.svelte';
  import { formatBytes, modName } from '../lib/preview';
  import { safeUrl } from '../lib/markdown';
  import type { DepEdge, MissingReq } from '../lib/preview';
  import type { ModEntry } from '../lib/types';

  let {
    mod,
    enabled,
    locked,
    onToggle,
    edges = [],
    missing = [],
    conflicts = [],
    alt = false,
  }: {
    mod: ModEntry;
    enabled: boolean;
    locked: boolean;
    onToggle: (next: boolean) => void;
    edges?: DepEdge[];
    missing?: MissingReq[];
    conflicts?: string[];
    alt?: boolean;
  } = $props();

  let expanded = $state(false);
  const d = $derived(mod.display);
  const depCount = $derived(edges.length + missing.length);
  // side badge for the toggleable classes; `required` is already the lock chip
  const presenceKey = {
    optional_client: 'mr.presenceClient',
    optional_server: 'mr.presenceServer',
    optional_both: 'mr.presenceBoth',
    coremod: 'mr.presenceCoremod',
  } as const;
  const presence = $derived(
    d?.presence && d.presence !== 'required' ? presenceKey[d.presence] : null,
  );
</script>

<div class="row" class:alt class:off={!enabled}>
  <input
    class="chk"
    type="checkbox"
    checked={enabled}
    disabled={locked}
    aria-label={modName(mod)}
    title={locked ? t('mr.lockedHint') : t('mr.optionalHint')}
    onchange={(e) => onToggle(e.currentTarget.checked)}
  />
  <ModIcon name={mod.filename} iconUrl={d?.icon_url} source={mod.source} sha1={mod.sha1} size={34} mono />
  <div class="meta">
    <div class="l1">
      <span class="nm">{modName(mod)}</span>
      {#if d?.category}<span class="chip cat">{d.category}</span>{/if}
      {#if locked}<span class="chip req">{t('mr.required')}</span>{:else}<span class="chip opt"
          >{t('mr.optional')}</span
        >{/if}
      {#if presence}<span class="chip side">{t(presence)}</span>{/if}
      {#if d?.license}<span class="chip lic">{d.license}</span>{/if}
      {#if conflicts.length}<span class="chip warn" title={t('mr.conflictsWith', { list: conflicts.join(', ') })}
          >{t('mr.conflicts')}</span
        >{/if}
    </div>
    <div class="l2 mono">
      <span>{mod.filename}</span>
      <span class="faint">{formatBytes(mod.size_bytes)}</span>
      {#if mod.source.type === 'modrinth'}<span class="faint">modrinth</span>{/if}
      {#if d?.url}<a href={safeUrl(d.url)} target="_blank" rel="noopener noreferrer">{t('mr.learnMore')}</a>{/if}
    </div>
    {#if d?.description}<div class="desc">{d.description}</div>{/if}
  </div>
  {#if depCount > 0}
    <button class="exp" class:open={expanded} title={t('mr.depsHint')} onclick={() => (expanded = !expanded)}>
      {t('mr.deps', { n: depCount })}<span class="caret"></span>
    </button>
  {/if}
</div>
{#if expanded && depCount > 0}
  <div class="tree">
    {#each edges as e (e.to)}
      <div class="dep">
        <span class="dot ok"></span>
        <span class="mono">{e.to}</span>
        {#if e.versionRange}<span class="chip vr mono">{e.versionRange}</span>{/if}
        {#if e.optional}<span class="chip optdep">{t('mr.optional')}</span>{/if}
      </div>
    {/each}
    {#each missing as m (m.requires)}
      <div class="dep">
        <span class="dot bad"></span>
        <span class="mono">{m.requires}</span>
        <span class="chip warn">{t('mr.missing')}</span>
      </div>
    {/each}
  </div>
{/if}

<style>
  .row {
    display: flex;
    align-items: flex-start;
    gap: 11px;
    padding: 9px 12px;
    border: 1px solid var(--p-outline);
    border-radius: 8px;
    background: var(--p-surface);
  }
  .row.alt {
    opacity: 0.62;
  }
  .row.off {
    opacity: 0.4;
  }
  .chk {
    margin-top: 9px;
    width: auto;
    height: auto;
    appearance: auto;
    -webkit-appearance: auto;
    accent-color: var(--p-accent);
  }
  .meta {
    flex: 1;
    min-width: 0;
  }
  .l1 {
    display: flex;
    align-items: center;
    gap: 7px;
    flex-wrap: wrap;
  }
  .nm {
    font-size: var(--fs-md);
    font-weight: 600;
    color: var(--p-fg);
  }
  .l2 {
    display: flex;
    gap: 12px;
    font-size: var(--fs-xs);
    color: var(--p-fg-dim);
    margin-top: 3px;
  }
  .l2 a {
    color: var(--p-accent);
    text-decoration: none;
  }
  .faint {
    opacity: 0.6;
  }
  .desc {
    font-size: var(--fs-sm);
    color: var(--p-fg-dim);
    margin-top: 5px;
    line-height: 1.45;
  }
  .chip {
    font-size: var(--fs-xs);
    padding: 1px 7px;
    border-radius: 999px;
    border: 1px solid var(--p-outline);
    color: var(--p-fg-dim);
    white-space: nowrap;
  }
  .chip.cat {
    color: var(--p-accent);
    border-color: color-mix(in srgb, var(--p-accent) 45%, transparent);
  }
  .chip.req {
    color: #8aa0c8;
  }
  .chip.opt {
    color: var(--p-ok);
    border-color: color-mix(in srgb, var(--p-ok) 40%, transparent);
  }
  .chip.warn {
    color: var(--p-danger);
    border-color: color-mix(in srgb, var(--p-danger) 50%, transparent);
  }
  .chip.side {
    color: var(--p-fg-dim);
    border-style: dashed;
  }
  .exp {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 7px;
    background: transparent;
    border: 1px solid var(--p-outline);
    color: var(--p-fg-dim);
    font-size: var(--fs-xs);
    padding: 4px 9px;
    border-radius: 6px;
    cursor: pointer;
  }
  .exp:hover {
    border-color: var(--p-accent);
    color: var(--p-fg);
  }
  .caret {
    width: 0;
    height: 0;
    border-left: 4px solid transparent;
    border-right: 4px solid transparent;
    border-top: 5px solid currentColor;
    transition: transform var(--dur-state) var(--ease-out);
  }
  .exp.open .caret {
    transform: rotate(180deg);
  }
  .tree {
    margin: 2px 0 2px 58px;
    padding: 8px 12px;
    border-left: 2px solid var(--p-outline);
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .dep {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--fs-sm);
    color: var(--p-fg);
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex: none;
  }
  .dot.ok {
    background: var(--p-accent);
  }
  .dot.bad {
    background: var(--p-danger);
  }
  .chip.vr {
    color: var(--p-fg-dim);
  }
  .chip.optdep {
    color: var(--p-fg-dim);
    opacity: 0.8;
  }
</style>
