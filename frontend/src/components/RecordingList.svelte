<script lang="ts">
  import { fmtDuration, groupByDay } from '../lib/format'
  import type { Row } from '../lib/recordings'
  import Icon from './Icon.svelte'

  let {
    rows,
    selected,
    canTranscribe,
    onselect,
    ontranscribe,
    oncancel,
    ondelete,
  }: {
    rows: Row[]
    selected: string | null
    canTranscribe: boolean
    onselect: (id: string) => void
    ontranscribe: (id: string) => void
    oncancel: (jobId: number) => void
    ondelete: (id: string) => void
  } = $props()

  const groups = $derived(groupByDay(rows, new Date()))
  const active = (r: Row) => r.job && (r.job.state === 'queued' || r.job.state === 'running')
  const clock = (r: Row) => r.started.slice(11) || r.file
  const who = (r: Row) => (r.apps.length ? r.apps.join(', ') : r.trigger === 'manual' ? 'Manual' : 'Recording')
</script>

{#each groups as g (g.label)}
  <div class="sticky top-0 z-10 bg-panel px-4 pt-3 pb-1.5 text-[11.5px] font-semibold tracking-wider text-faint uppercase">{g.label}</div>
  {#each g.items as r (r.id)}
    <div
      class={['mx-2 mb-0.5 cursor-pointer rounded-[9px] px-2.5 py-2 -outline-offset-2 transition-colors', r.id === selected ? 'bg-sel' : 'hover:bg-panel-2']}
      role="button"
      tabindex="0"
      onclick={() => onselect(r.id)}
      onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), onselect(r.id))}
    >
      <div class="flex items-baseline gap-2.5">
        <span class="font-semibold tabular-nums">{clock(r)}</span>
        <span class="min-w-0 flex-1 truncate text-muted">{who(r)}</span>
        <span class="text-[12.5px] text-muted tabular-nums">{fmtDuration(r.duration_secs)}</span>
      </div>
      <div class="mt-1 flex min-h-[26px] flex-wrap items-center gap-1.5 empty:hidden">
        {#if active(r)}
          <div class="h-[5px] flex-1 overflow-hidden rounded-full bg-line" title={r.job!.phase}>
            <div class="h-full bg-accent transition-[width] duration-500" style={`width:${Math.round(r.job!.progress * 100)}%`}></div>
          </div>
          <span class="min-w-10 text-xs text-muted tabular-nums">{r.job!.state === 'queued' ? 'Queued' : `${Math.round(r.job!.progress * 100)}%`}</span>
          <button class="btn btn-sm" onclick={(e) => (e.stopPropagation(), oncancel(r.job!.id))}>Cancel</button>
        {:else}
          {#if r.transcribed}
            <span class="inline-flex items-center gap-1 rounded-full bg-ok/12 px-2 py-0.5 text-[11.5px] font-medium text-ok"><Icon name="check" size={13} /> Transcript</span>
          {/if}
          {#if r.status === 'recovered'}
            <span class="rounded-full bg-warn-soft px-2 py-0.5 text-[11.5px] font-medium text-warn">Recovered</span>
          {/if}
          {#if r.job?.state === 'error'}
            <span class="inline-flex items-center gap-1 rounded-full bg-danger/12 px-2 py-0.5 text-[11.5px] font-medium text-danger" title={r.job.error}
              ><Icon name="alert" size={13} /> Failed</span
            >
          {/if}
          {#if canTranscribe && !r.transcribed}
            <button class="btn btn-sm" onclick={(e) => (e.stopPropagation(), ontranscribe(r.id))}>Transcribe</button>
          {:else if canTranscribe && r.transcribed && r.id === selected}
            <button class="btn btn-sm border-transparent bg-transparent shadow-none" onclick={(e) => (e.stopPropagation(), ontranscribe(r.id))}>Redo</button>
          {/if}
          <!-- Not while it is being transcribed: the transcriber has the file open. -->
          <button
            class="icon-btn ml-auto size-[26px] hover:enabled:text-danger"
            title="Delete this recording"
            aria-label="Delete this recording"
            onclick={(e) => (e.stopPropagation(), ondelete(r.id))}
            onkeydown={(e) => e.stopPropagation()}><Icon name="trash" size={15} /></button
          >
        {/if}
      </div>
    </div>
  {/each}
{/each}
