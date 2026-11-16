<script lang="ts">
  import { local } from '../lib/api'
  import { fmtTime } from '../lib/format'
  import { parseTranscript, type Transcript } from '../lib/transcript'
  import Icon from './Icon.svelte'
  import Segmented from './Segmented.svelte'

  let {
    id,
    transcribed,
    version = null,
    busy = false,
    time,
    playing,
    onseek,
    ondelete,
  }: {
    id: string
    transcribed: boolean
    version?: number | null
    busy?: boolean
    time: number
    playing: boolean
    onseek: (t: number) => void
    ondelete: () => void
  } = $props()

  let transcript = $state<Transcript | null>(null)
  let loadError = $state('')
  let filter = $state<'all' | 'me' | 'others'>('all')
  let rows: HTMLElement[] = $state([])

  // The parent rebuilds the recording object on every poll, which reruns an
  // effect reading the props directly; a derived only fires when its value changes.
  const recId = $derived(id)
  const fileVersion = $derived(version)
  const has = $derived(transcribed)
  let shownFor = ''

  // Reloads when the recording changes or its transcript file is rewritten (Redo).
  $effect(() => {
    const want = recId
    void fileVersion
    loadError = ''
    // A redone transcript replaces the old one in place, without a blank in between.
    if (!has || want !== shownFor) transcript = null
    shownFor = want
    if (!has) return
    local
      .transcript(want)
      .then((md) => {
        if (want === id) transcript = md == null ? null : parseTranscript(md)
      })
      .catch((e) => {
        if (want === id) loadError = e.message
      })
  })

  const segments = $derived((transcript?.segments ?? []).filter((s) => filter === 'all' || s.speaker === filter))

  // The line being played: the last one that started before now.
  const current = $derived.by(() => {
    if (!playing && time <= 0) return -1
    let idx = -1
    for (let i = 0; i < segments.length; i++) {
      if (segments[i].start <= time + 0.25) idx = i
      else break
    }
    return idx
  })

  $effect(() => {
    if (current >= 0 && playing) rows[current]?.scrollIntoView({ block: 'nearest', behavior: 'smooth' })
  })

  const meta = $derived(
    transcript ? [transcript.fields['Duration'] && `Duration ${transcript.fields['Duration']}`, transcript.fields['Transcribed with']].filter(Boolean).join(' · ') : '',
  )
</script>

{#if !transcribed}
  <div class="mt-5 flex items-start gap-3.5 rounded-xl border border-dashed border-line-strong p-5 text-muted">
    <Icon name="text" size={22} />
    <div>
      {#if busy}
        <strong class="text-fg">Transcribing…</strong>
        <p class="mt-1">The transcript appears here when it is done. You can play the recording meanwhile.</p>
      {:else}
        <strong class="text-fg">Not transcribed yet</strong>
        <p class="mt-1">You can still play it above. Use <em>Transcribe</em> beside it in the list to make a transcript.</p>
      {/if}
    </div>
  </div>
{:else if loadError}
  <div class="hint mt-5"><Icon name="alert" size={16} /> Could not read the transcript: {loadError}</div>
{:else if !transcript}
  <div class="py-6 text-muted">Loading transcript…</div>
{:else}
  {#if busy}
    <div class="hint hint-neutral mt-5">Transcribing again… this transcript is replaced when it is done.</div>
  {/if}
  <div class="mt-5 mb-2 flex flex-wrap items-center justify-between gap-3">
    <div class="text-[12.5px] text-muted">{meta}</div>
    <div class="flex items-center gap-2">
      <Segmented
        small
        label="Show"
        bind:value={filter}
        options={[
          { value: 'all', label: 'Both' },
          { value: 'me', label: 'Me' },
          { value: 'others', label: 'Others' },
        ]}
      />
      <button class="btn btn-sm hover:enabled:border-danger hover:enabled:text-danger" title="Delete this transcript (the recording stays)" disabled={busy} onclick={ondelete}>
        <Icon name="trash" size={13} /> Delete
      </button>
    </div>
  </div>
  {#if segments.length === 0}
    <div class="py-12 text-center text-muted">No speech detected.</div>
  {:else}
    <ol class="m-0 list-none p-0">
      {#each segments as s, i (i + ':' + s.start + s.speaker)}
        <li
          bind:this={rows[i]}
          class={[
            'grid grid-cols-[56px_1fr] gap-x-2.5 gap-y-1 rounded-lg px-2.5 py-2 transition-colors lg:grid-cols-[62px_64px_1fr]',
            i === current ? 'bg-sel' : 'hover:bg-panel-2',
          ]}
        >
          <button
            class="self-start pt-px text-left text-[12.5px] text-muted tabular-nums hover:text-accent hover:underline"
            title="Play from here"
            onclick={() => onseek(s.start)}>{fmtTime(s.start)}</button
          >
          <span
            class={[
              'mt-px self-start justify-self-start rounded-full px-2 py-px text-[11.5px] font-semibold',
              s.speaker === 'me' ? 'bg-me-soft text-me' : 'bg-others-soft text-others',
            ]}>{s.speaker === 'me' ? 'Me' : 'Others'}</span
          >
          <span class="col-span-2 leading-[1.55] lg:col-span-1">{s.text}</span>
        </li>
      {/each}
    </ol>
  {/if}
{/if}
