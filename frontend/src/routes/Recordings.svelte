<script lang="ts">
  import { onMount } from 'svelte'
  import Icon from '../components/Icon.svelte'
  import Player from '../components/Player.svelte'
  import RecordingList from '../components/RecordingList.svelte'
  import ServiceButton from '../components/ServiceButton.svelte'
  import TranscriptView from '../components/TranscriptView.svelte'
  import { local, transcriber } from '../lib/api'
  import { fmtDuration } from '../lib/format'
  import { mergeRecordings, sameFolder, type LocalRecording } from '../lib/recordings'
  import type { createTranscriber } from '../lib/transcriber.svelte'

  let { tx, active = true }: { tx: ReturnType<typeof createTranscriber>; active?: boolean } = $props()

  let recordings = $state<LocalRecording[]>([])
  let outputDir = $state('')
  let loaded = $state(false)
  let loadError = $state('')
  let actionError = $state('')
  let selected = $state<string | null>(null)
  let time = $state(0)
  let playing = $state(false)
  let player: Player | undefined = $state()

  async function refresh() {
    try {
      const r = await local.recordings()
      recordings = r.recordings
      outputDir = r.output_dir
      loadError = ''
      if (!selected || !recordings.some((x) => x.id === selected)) selected = recordings[0]?.id ?? null
    } catch (e) {
      loadError = (e as Error).message
    }
    loaded = true
  }

  onMount(() => {
    void refresh()
    const onFocus = () => void refresh()
    window.addEventListener('focus', onFocus)
    return () => window.removeEventListener('focus', onFocus)
  })

  // Back from Settings: the recordings folder may have changed.
  let wasActive = true // onMount does the first load
  $effect(() => {
    if (active && !wasActive) void refresh()
    wasActive = active
  })

  // A transcription finished: its .md is on disk now.
  $effect(() => {
    if (tx.current.finishedJobIds.length) void refresh()
  })

  const t = $derived(tx.current)
  const rows = $derived(mergeRecordings(recordings, t.online ? (t.state?.jobs ?? []) : []))
  const rec = $derived(rows.find((r) => r.id === selected) ?? null)
  const folderMismatch = $derived(!!(t.online && t.state && outputDir && !sameFolder(t.state.recordings_dir, outputDir)))
  const modelReady = $derived(!!t.state?.engine.active)
  const canTranscribe = $derived(t.online && modelReady && !folderMismatch)

  async function act(f: () => Promise<unknown>) {
    actionError = ''
    try {
      await f()
      await tx.poll()
    } catch (e) {
      actionError = (e as Error).message
    }
  }

  const title = (r: NonNullable<typeof rec>) => [r.started || r.file, r.apps.join(', ')].filter(Boolean).join(' · ')
</script>

<div class="grid h-full min-h-0 grid-cols-[280px_1fr] lg:grid-cols-[minmax(300px,360px)_1fr]">
  <aside class="flex min-h-0 flex-col border-r border-line bg-panel">
    <header class="flex items-center justify-between pt-3.5 pr-2.5 pb-1.5 pl-[18px]">
      <h2 class="m-0 text-[15px] font-semibold">Recordings <span class="ml-1 font-medium text-faint">{recordings.length || ''}</span></h2>
      <div class="flex gap-0.5">
        <button class="icon-btn" title="Refresh" aria-label="Refresh" onclick={refresh}><Icon name="refresh" /></button>
        <button class="icon-btn" title="Open recordings folder" aria-label="Open recordings folder" onclick={() => act(() => local.openFolder())}>
          <Icon name="folder" />
        </button>
      </div>
    </header>

    <div class="flex flex-col gap-1.5 px-3 [&:not(:empty)]:pb-1.5">
      {#if !t.online}
        <div class="hint hint-neutral items-center">
          <span class="flex-1">{tx.service.phase === 'starting' ? 'Starting the transcriber…' : 'Transcription is offline.'}</span>
          <ServiceButton {tx} small />
        </div>
        {#if tx.service.failure}
          <div class="hint"><Icon name="alert" size={16} /> <span class="whitespace-pre-wrap">{tx.service.failure}</span></div>
        {/if}
      {:else if folderMismatch}
        <div class="hint">
          <Icon name="alert" size={16} />
          <span
            >The transcriber reads <code class="font-mono text-[11.5px] break-all">{t.state?.recordings_dir}</code> but MeetRec saves to
            <code class="font-mono text-[11.5px] break-all">{outputDir}</code>. Point both at the same folder to transcribe.</span
          >
        </div>
      {:else if !modelReady}
        <div class="hint hint-neutral">Load a transcription model in Settings to transcribe.</div>
      {/if}
      {#if actionError}
        <div class="hint"><Icon name="alert" size={16} /> {actionError}</div>
      {/if}
    </div>

    <div class="flex-1 overflow-y-auto pb-3">
      {#if loadError}
        <div class="px-6 py-12 text-center text-muted">Could not list recordings: {loadError}</div>
      {:else if loaded && rows.length === 0}
        <div class="px-6 py-12 text-center text-muted">No recordings yet in<br /><code class="font-mono text-[11.5px] break-all">{outputDir}</code></div>
      {:else}
        <RecordingList
          {rows}
          {selected}
          {canTranscribe}
          onselect={(id) => (selected = id)}
          ontranscribe={(id) => act(() => transcriber.transcribe(id))}
          oncancel={(jobId) => act(() => transcriber.cancel(jobId))}
        />
      {/if}
    </div>
  </aside>

  <section class="min-w-0 overflow-y-auto px-5 pt-[18px] pb-10 lg:px-8 lg:pt-[22px] lg:pb-12">
    {#if rec}
      <h1 class="m-0 text-[21px] font-semibold tracking-tight">{title(rec)}</h1>
      <div class="mt-2.5 mb-4 flex flex-wrap gap-1.5 text-xs">
        {#if rec.duration_secs != null}<span class="rounded-full border border-line bg-panel px-2.5 py-0.5 text-muted">{fmtDuration(rec.duration_secs)}</span>{/if}
        {#if rec.trigger}<span class="rounded-full border border-line bg-panel px-2.5 py-0.5 text-muted">{rec.trigger === 'auto' ? 'Auto-recorded' : 'Manual'}</span>{/if}
        {#if rec.status === 'recovered'}<span class="rounded-full bg-warn-soft px-2.5 py-0.5 text-warn">Recovered after a crash</span>{/if}
        <span class="rounded-full border border-line bg-panel px-2.5 py-0.5 text-muted"
          >{rec.speakers.length > 1 ? 'Me + Others' : rec.speakers[0] === 'others' ? 'Others only' : 'Me only'}</span
        >
        <button class="rounded-full border border-line bg-panel px-2.5 py-0.5 text-accent hover:border-accent" onclick={() => act(() => local.openFolder(rec.id))}
          >Show in folder</button
        >
      </div>
      <div class="sticky -top-[22px] z-20 -mt-2 bg-linear-to-b from-bg from-85% to-transparent py-2">
        <Player bind:this={player} id={rec.id} speakers={rec.speakers} bind:time bind:playing />
      </div>
      <TranscriptView
        id={rec.id}
        transcribed={rec.transcribed}
        version={rec.transcript_mtime}
        busy={rec.job?.state === 'queued' || rec.job?.state === 'running'}
        {time} {playing} onseek={(s) => player?.seek(s)} />
    {:else if loaded}
      <div class="px-6 py-12 text-center text-muted">Select a recording.</div>
    {/if}
  </section>
</div>
