<script lang="ts">
  import { t } from '../lib/i18n.svelte';
  import type { ResolveReport } from '../lib/types';

  let {
    report,
    onSuggest,
  }: { report: ResolveReport; onSuggest?: (selector: string) => void } = $props();

  // a clean run has nothing in any of the finding lists
  const clean = $derived(
    report.missing.length === 0 &&
      report.conflicts.length === 0 &&
      report.version_issues.length === 0 &&
      report.overlaps.length === 0 &&
      report.loader_mismatch.length === 0 &&
      report.loader_version_issues.length === 0 &&
      report.mixin_gaps.length === 0 &&
      report.unresolved.length === 0 &&
      report.forced_client_attempts.length === 0,
  );
</script>

<div class="panel resrep">
  <div class="rhead">
    <span class="faint">{t('resolve.resolved', { n: report.resolved_mods, total: report.declared_mods })}</span>
    {#if report.missing.length}<span class="pill danger">{t('resolve.missing', { n: report.missing.length })}</span>{/if}
    {#if report.conflicts.length}<span class="pill danger">{t('resolve.conflicts', { n: report.conflicts.length })}</span>{/if}
    {#if report.optional_conflicts.length}<span class="pill warn">{t('resolve.optConflicts', { n: report.optional_conflicts.length })}</span>{/if}
    {#if report.version_issues.length}<span class="pill warn">{t('resolve.versionIssues', { n: report.version_issues.length })}</span>{/if}
    {#if report.overlaps.length}<span class="pill warn">{t('resolve.overlaps', { n: report.overlaps.length })}</span>{/if}
    {#if report.loader_mismatch.length}<span class="pill danger">{t('resolve.loaderMismatch', { n: report.loader_mismatch.length })}</span>{/if}
    {#if report.loader_version_issues.length}<span class="pill danger">{t('resolve.loaderWindow', { n: report.loader_version_issues.length })}</span>{/if}
    {#if report.mixin_gaps.length}<span class="pill danger">{t('resolve.mixinGaps', { n: report.mixin_gaps.length })}</span>{/if}
    {#if report.loader_bridged.length}<span class="pill faint">{t('resolve.loaderBridged', { n: report.loader_bridged.length })}</span>{/if}
    {#if report.not_on_loader.length}<span class="pill faint">{t('resolve.notOnLoader', { n: report.not_on_loader.length })}</span>{/if}
    {#if report.unresolved.length}<span class="pill faint">{t('resolve.unresolved', { n: report.unresolved.length })}</span>{/if}
    {#if report.forced_client_attempts.length}<span class="pill danger">{t('resolve.forcedClient', { n: report.forced_client_attempts.length })}</span>{/if}
    {#if report.side_disagreements.length}<span class="pill warn">{t('resolve.sideDis', { n: report.side_disagreements.length })}</span>{/if}
    {#if report.coremods.length}<span class="pill faint">{t('resolve.coremods', { n: report.coremods.length })}</span>{/if}
    {#if report.unclassified.length}<span class="pill faint">{t('resolve.unclassified', { n: report.unclassified.length })}</span>{/if}
    {#if report.server_side.length}<span class="pill faint">{t('resolve.serverSide', { n: report.server_side.length })}</span>{/if}
    {#if report.suggestions.length}<span class="pill faint">{t('resolve.suggestions', { n: report.suggestions.length })}</span>{/if}
    {#if clean}<span class="pill ok">{t('resolve.clean')}</span>{/if}
  </div>

  {#if report.missing.length}
    <div class="rlist">
      <div class="rl-h danger">{t('resolve.missingH')}</div>
      {#each report.missing as m}
        <div class="rl-row">
          <span class="mono strong">{m.target}</span>
          {#if m.version_range}<span class="mono faint">{m.version_range}</span>{/if}
          <span class="faint">{t('resolve.neededBy', { who: m.needed_by.join(', ') })}</span>
          {#if m.reason === 'external'}<span class="pill warn">{t('resolve.external')}</span>{/if}
          <span class="src mono">{m.source}</span>
        </div>
      {/each}
    </div>
  {/if}

  {#if report.mixin_gaps.length}
    <div class="rlist">
      <div class="rl-h danger">{t('resolve.mixinGapsH')}</div>
      {#each report.mixin_gaps as g}
        <div class="rl-row">
          <span class="mono strong">{g.filename}</span>
          <span class="faint">{t('resolve.patches')}</span>
          <span class="mono">{g.needed}</span>
          <span class="faint">{t('resolve.absentFrom', { owner: g.owner })}</span>
          <span class="src mono">{g.config}</span>
        </div>
      {/each}
    </div>
  {/if}

  {#if report.forced_client_attempts.length}
    <div class="rlist">
      <div class="rl-h danger">{t('resolve.forcedClientH')}</div>
      {#each report.forced_client_attempts as f}
        <div class="rl-row">
          <span class="mono strong">{f.filename}</span>
          <span class="faint">{t('resolve.neededBy', { who: f.needed_by.join(', ') })}</span>
          <span class="src mono">{f.source}</span>
        </div>
      {/each}
    </div>
  {/if}

  {#if report.side_disagreements.length}
    <div class="rlist">
      <div class="rl-h warn">{t('resolve.sideDisH')}</div>
      {#each report.side_disagreements as d}
        <div class="rl-row">
          <span class="mono strong">{d.filename}</span>
          <span class="faint">{t('resolve.sideDisRow', { mr: d.modrinth_side, bc: d.bytecode_side })}</span>
        </div>
      {/each}
    </div>
  {/if}

  {#if report.conflicts.length}
    <div class="rlist">
      <div class="rl-h danger">{t('resolve.conflictsH')}</div>
      {#each report.conflicts as c}
        <div class="rl-row">
          <span class="mono strong">{c.a}</span>
          <span class="faint">{c.hard ? t('resolve.incompatible') : t('resolve.discouraged')}</span>
          <span class="mono strong">{c.b}</span>
          <span class="src mono">{c.source}</span>
        </div>
      {/each}
    </div>
  {/if}

  {#if report.optional_conflicts.length}
    <div class="rlist">
      <div class="rl-h warn">{t('resolve.optConflictsH')}</div>
      {#each report.optional_conflicts as c}
        <div class="rl-row">
          <span class="mono strong">{c.a}</span>
          <span class="faint">{c.hard ? t('resolve.incompatible') : t('resolve.discouraged')}</span>
          <span class="mono strong">{c.b}</span>
          <span class="faint">{t('resolve.ifEnabled')}</span>
          <span class="src mono">{c.source}</span>
        </div>
      {/each}
    </div>
  {/if}

  {#if report.loader_mismatch.length}
    <div class="rlist">
      <div class="rl-h danger">{t('resolve.loaderMismatchH')}</div>
      {#each report.loader_mismatch as l}
        <div class="rl-row">
          <span class="mono strong">{l.filename}</span>
          <span class="faint">{t('resolve.builtFor', { loaders: l.artifact_loaders.join(', '), pack: l.pack_loader })}</span>
        </div>
      {/each}
    </div>
  {/if}

  {#if report.loader_bridged.length}
    <div class="rlist">
      <div class="rl-h faint">{t('resolve.loaderBridgedH')}</div>
      {#each report.loader_bridged as l}
        <div class="rl-row">
          <span class="mono strong">{l.filename}</span>
          <span class="faint">{t('resolve.bridgedBy', { loaders: l.artifact_loaders.join(', '), by: l.bridged_by ?? '' })}</span>
        </div>
      {/each}
    </div>
  {/if}

  {#if report.not_on_loader.length}
    <div class="rlist">
      <div class="rl-h faint">{t('resolve.notOnLoaderH')}</div>
      {#each report.not_on_loader as m}
        <div class="rl-row">
          <span class="mono strong">{m.target}</span>
          <span class="faint">{t('resolve.neededBy', { who: m.needed_by.join(', ') })}</span>
          <span class="src mono">{m.source}</span>
        </div>
      {/each}
    </div>
  {/if}

  {#if report.loader_version_issues.length}
    <div class="rlist">
      <div class="rl-h danger">{t('resolve.loaderWindowH')}</div>
      {#each report.loader_version_issues as l}
        <div class="rl-row">
          <span class="mono strong">{l.filename}</span>
          <span class="faint">{t('resolve.loaderNeeds', { loader: l.loader, range: l.required_range, pinned: l.pack_version })}</span>
        </div>
      {/each}
    </div>
  {/if}

  {#if report.version_issues.length}
    <div class="rlist">
      <div class="rl-h warn">{t('resolve.versionIssuesH')}</div>
      {#each report.version_issues as v}
        <div class="rl-row">
          <span class="mono strong">{v.filename}</span>
          <span class="faint">{t('resolve.ships', { v: v.present_version })}</span>
          <span class="mono">{v.required_range}</span>
          <span class="faint">{t('resolve.neededBy', { who: v.needed_by.join(', ') })}</span>
        </div>
      {/each}
    </div>
  {/if}

  {#if report.overlaps.length}
    <div class="rlist">
      <div class="rl-h warn">{t('resolve.overlapsH')}</div>
      {#each report.overlaps as o}
        <div class="rl-row">
          <span class="mono strong">{o.capability}</span>
          <span class="faint">{o.mods.join(', ')}</span>
        </div>
      {/each}
    </div>
  {/if}

  {#if report.unresolved.length}
    <div class="rlist">
      <div class="rl-h faint">{t('resolve.unresolvedH')}</div>
      {#each report.unresolved as u}
        <div class="rl-row"><span class="mono">{u}</span></div>
      {/each}
    </div>
  {/if}

  {#if report.server_side.length}
    <div class="rlist">
      <div class="rl-h faint">{t('resolve.serverSideH')}</div>
      {#each report.server_side as f}
        <div class="rl-row"><span class="mono">{f}</span></div>
      {/each}
    </div>
  {/if}

  {#if report.coremods.length}
    <div class="rlist">
      <div class="rl-h faint">{t('resolve.coremodsH')}</div>
      {#each report.coremods as f}
        <div class="rl-row"><span class="mono">{f}</span></div>
      {/each}
    </div>
  {/if}

  {#if report.unclassified.length}
    <div class="rlist">
      <div class="rl-h faint">{t('resolve.unclassifiedH')}</div>
      {#each report.unclassified as f}
        <div class="rl-row"><span class="mono">{f}</span></div>
      {/each}
    </div>
  {/if}

  {#if report.suggestions.length}
    <div class="rlist">
      <div class="rl-h faint">{t('resolve.suggestionsH')}</div>
      {#each report.suggestions as sel}
        <div class="rl-row">
          <span class="mono">{sel}</span>
          {#if onSuggest}
            <button class="add" onclick={() => onSuggest(sel)}>{t('resolve.addSuggested')}</button>
          {/if}
        </div>
      {/each}
    </div>
  {/if}

  {#if report.version_windows_unchecked}
    <div class="rfoot faint">{t('resolve.unchecked', { n: report.version_windows_unchecked })}</div>
  {/if}
</div>

<style>
  .resrep {
    padding: var(--space-3);
    margin-bottom: var(--space-4);
  }
  .rhead {
    display: flex;
    gap: var(--space-2) var(--space-3);
    flex-wrap: wrap;
    align-items: center;
    font-size: var(--fs-sm);
  }
  .pill {
    font-size: var(--fs-xs);
    padding: 1px 8px;
    border-radius: 999px;
    border: 1px solid var(--seam);
    white-space: nowrap;
  }
  .pill.danger {
    color: var(--danger);
    border-color: color-mix(in srgb, var(--danger) 40%, transparent);
    background: var(--danger-soft);
  }
  .pill.warn {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 40%, transparent);
    background: var(--warn-soft);
  }
  .pill.ok {
    color: var(--ok);
    border-color: color-mix(in srgb, var(--ok) 40%, transparent);
  }
  .pill.faint {
    color: var(--fg-dim);
  }
  .rlist {
    margin-top: var(--space-3);
  }
  .rl-h {
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin-bottom: 6px;
  }
  .rl-h.danger {
    color: var(--danger);
  }
  .rl-h.warn {
    color: var(--warn);
  }
  .rl-h.faint {
    color: var(--fg-dim);
  }
  .rl-row {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--space-2) var(--space-3);
    font-size: var(--fs-sm);
    padding: 3px 0;
    border-top: 1px solid var(--seam);
  }
  .rl-row:first-of-type {
    border-top: none;
  }
  .strong {
    font-weight: 600;
  }
  .src {
    margin-left: auto;
    font-size: var(--fs-xs);
    color: var(--fg-dim);
    opacity: 0.7;
  }
  .rfoot {
    margin-top: var(--space-3);
    font-size: var(--fs-xs);
  }
  .add {
    background: transparent;
    border: 1px solid var(--seam);
    color: var(--fg-dim);
    font-size: var(--fs-xs);
    padding: 1px 8px;
    border-radius: 999px;
    cursor: pointer;
  }
  .add:hover {
    border-color: var(--accent, var(--fg));
    color: var(--fg);
  }
</style>
