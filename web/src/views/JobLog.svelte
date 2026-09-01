<script lang="ts">
  import { api } from '../lib/api';
  import { t } from '../lib/i18n.svelte';
  import type { JobStatus } from '../lib/types';

  let { jobId, onDone }: { jobId: string; onDone?: (status: JobStatus) => void } = $props();

  let lines = $state<string[]>([]);
  let status = $state<JobStatus>('running');
  const statusLabel = $derived(
    t(status === 'done' ? 'job.done' : status === 'failed' ? 'job.failed' : 'job.running'),
  );

  $effect(() => {
    // Re-subscribe when jobId changes; reset for the new job.
    lines = [];
    status = 'running';
    const source = new EventSource(api.jobEventsUrl(jobId));
    const finish = (s: JobStatus) => {
      status = s;
      source.close();
      onDone?.(s);
    };
    source.addEventListener('line', (ev) => {
      lines = [...lines, (ev as MessageEvent).data];
    });
    source.addEventListener('done', () => finish('done'));
    source.addEventListener('failed', () => finish('failed'));
    source.onerror = () => {
      if (status === 'running') {
        lines = [...lines, t('job.interrupted')];
        finish('failed');
      }
    };
    return () => source.close();
  });
</script>

<div class="jl">
  <div class="head">
    <span class="dot" class:live={status === 'running'} class:ok={status === 'done'} class:bad={status === 'failed'} aria-hidden="true"></span>
    <span class="st mono" class:ok={status === 'done'} class:bad={status === 'failed'}>{statusLabel}</span>
  </div>
  {#if lines.length}
    <pre class="log mono">{lines.join('\n')}</pre>
  {:else if status === 'running'}
    <!-- A job that has not spoken yet still has to look like it is running. The
         status word on its own reads as a stray label rather than as work in
         progress. -->
    <div class="quiet mono">{t('job.noOutputYet')}</div>
  {/if}
</div>

<style>
  /* Colours come from whatever this is dropped into. The panel is the default,
     and the pack preview repoints them because it paints in the launcher's
     palette rather than the panel's. */
  .jl {
    --jl-fg: var(--fg-dim);
    --jl-bg: var(--bg);
    --jl-seam: var(--seam);
    --jl-ok: var(--ok);
    --jl-bad: var(--danger);
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex: none;
    background: var(--jl-fg);
  }
  .dot.live {
    animation: jl-beat 1.4s ease-in-out infinite;
  }
  .dot.ok {
    background: var(--jl-ok);
  }
  .dot.bad {
    background: var(--jl-bad);
  }
  @keyframes jl-beat {
    0%,
    100% {
      opacity: 0.28;
    }
    50% {
      opacity: 1;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .dot.live {
      animation: none;
      opacity: 0.7;
    }
  }
  .st {
    font-size: var(--fs-sm);
    color: var(--jl-fg);
  }
  .st.ok {
    color: var(--jl-ok);
  }
  .st.bad {
    color: var(--jl-bad);
  }
  .quiet {
    margin-top: 8px;
    font-size: var(--fs-sm);
    color: var(--jl-fg);
    opacity: 0.55;
  }
  .log {
    background: var(--jl-bg);
    border: 1px solid var(--jl-seam);
    border-radius: var(--radius-sm);
    padding: 14px;
    margin: 10px 0 0;
    font-size: var(--fs-sm);
    line-height: 1.6;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 460px;
    overflow: auto;
  }
</style>
