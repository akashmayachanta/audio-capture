<script lang="ts">
  // Plays a recording as-is (Ogg Opus: one channel per speaker). Web Audio routes
  // each channel to both ears, so you don't hear yourself in one ear only, and
  // Listen picks Both / Me / Others.
  import { audioUrl } from '../lib/api'
  import { fmtTime } from '../lib/format'
  import { listenGains, type Listen } from '../lib/listen'
  import Icon from './Icon.svelte'
  import Segmented from './Segmented.svelte'

  let {
    id,
    speakers,
    time = $bindable(0),
    playing = $bindable(false),
  }: { id: string; speakers: string[]; time?: number; playing?: boolean } = $props()

  let audio: HTMLAudioElement
  let duration = $state(0)
  let paused = $state(true)
  let volume = $state(1)
  let scrub = $state<number | null>(null)
  let error = $state('')
  let listen = $state<Listen>('all')

  let ctx: AudioContext | null = null
  let gains: GainNode[] = []

  // Release the audio device when the player goes away.
  $effect(() => () => {
    void ctx?.close()
    ctx = null
  })

  $effect(() => {
    playing = !paused
  })

  // A new recording. Duration and position reset through the element's own
  // events when `src` changes; setting them here would race those events.
  // Through a derived: the parent rebuilds the recording object on every poll,
  // and reading `id` directly would cancel a drag on the seek bar each time.
  const recId = $derived(id)
  $effect(() => {
    void recId
    error = ''
    scrub = null
  })

  $effect(() => {
    const [a, b] = listenGains(speakers, listen)
    if (gains.length) {
      gains[0].gain.value = a
      gains[1].gain.value = b
    }
  })

  function ensureGraph() {
    if (ctx) return
    ctx = new AudioContext()
    const src = ctx.createMediaElementSource(audio)
    const split = ctx.createChannelSplitter(2)
    const merge = ctx.createChannelMerger(2)
    src.connect(split)
    gains = [ctx.createGain(), ctx.createGain()]
    gains.forEach((g, ch) => {
      split.connect(g, ch)
      g.connect(merge, 0, 0)
      g.connect(merge, 0, 1)
    })
    merge.connect(ctx.destination)
    const [a, b] = listenGains(speakers, listen)
    gains[0].gain.value = a
    gains[1].gain.value = b
  }

  export async function play() {
    ensureGraph()
    await ctx?.resume()
    try {
      await audio.play()
    } catch (e) {
      if ((e as Error).name !== 'AbortError') error = (e as Error).message
    }
  }

  export function seek(t: number, andPlay = true) {
    audio.currentTime = Math.max(0, t)
    time = audio.currentTime
    if (andPlay) void play()
  }

  function toggle() {
    if (paused) void play()
    else audio.pause()
  }

  function onError() {
    error = `This recording cannot be played here (${audio.error?.message || 'decode error'}).`
  }

  const shown = $derived(scrub ?? time)
  const pct = $derived(duration > 0 ? (shown / duration) * 100 : 0)
</script>

<div class="flex flex-wrap items-center gap-3.5 rounded-2xl border border-line bg-panel px-3.5 py-3 shadow-card">
  <audio
    bind:this={audio}
    src={audioUrl(id)}
    preload="metadata"
    bind:paused
    bind:currentTime={time}
    bind:duration
    bind:volume
    onerror={onError}
  ></audio>

  <button
    class="grid size-10 flex-none place-items-center rounded-full bg-accent pl-0.5 text-on-accent transition-transform hover:enabled:scale-105 disabled:opacity-40"
    onclick={toggle}
    aria-label={paused ? 'Play' : 'Pause'}
    disabled={!!error}
  >
    <Icon name={paused ? 'play' : 'pause'} size={paused ? 18 : 20} filled={paused} />
  </button>

  <div class="flex min-w-[220px] flex-1 items-center gap-3">
    <input
      class="range"
      type="range"
      min="0"
      max={duration || 0}
      step="0.1"
      value={shown}
      style={`--pct:${pct}%`}
      aria-label="Seek"
      disabled={!duration}
      oninput={(e) => (scrub = Number(e.currentTarget.value))}
      onchange={(e) => {
        seek(Number(e.currentTarget.value), !paused)
        scrub = null
      }}
    />
    <span class="text-[12.5px] whitespace-nowrap tabular-nums">{fmtTime(shown)} <span class="text-muted">/ {fmtTime(duration)}</span></span>
  </div>

  {#if speakers.length > 1}
    <Segmented
      small
      label="Listen"
      bind:value={listen}
      options={[
        { value: 'all', label: 'Both' },
        { value: 'me', label: 'Me' },
        { value: 'others', label: 'Others' },
      ]}
    />
  {/if}

  <div class="flex items-center gap-0.5">
    <button class="icon-btn" aria-label={volume === 0 ? 'Unmute' : 'Mute'} onclick={() => (volume = volume === 0 ? 1 : 0)}>
      <Icon name={volume === 0 ? 'mute' : 'volume'} />
    </button>
    <input class="range w-20 flex-none" type="range" min="0" max="1" step="0.05" bind:value={volume} style={`--pct:${volume * 100}%`} aria-label="Volume" />
  </div>

  {#if error}
    <div class="flex w-full items-center gap-1.5 text-[12.5px] text-danger"><Icon name="alert" size={15} /> {error}</div>
  {/if}
</div>
