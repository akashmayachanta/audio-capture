<script lang="ts">
  // Settings → Dictation: runs the dictation phrase cutter on the live mic (in
  // the window process) and shows what it hears. Nothing is recorded.
  import { onDestroy } from 'svelte'
  import Card from './Card.svelte'
  import Icon from './Icon.svelte'
  import { local, type MicView } from '../lib/api'
  import { levelDb, levelPct } from '../lib/meter'

  let { device, margin, active = true }: { device: string; margin: number; active?: boolean } = $props()

  let running = $state(false)
  let view = $state<MicView | null>(null)
  let error = $state('')
  let timer: ReturnType<typeof setTimeout> | undefined
  let generation = 0

  async function start() {
    error = ''
    const mine = ++generation
    try {
      await local.micTestStart(device, margin)
    } catch (e) {
      error = (e as Error).message
      return
    }
    running = true
    view = null
    poll(mine)
  }

  async function poll(mine: number) {
    if (mine !== generation || !running) return
    try {
      const { test } = await local.micTest()
      if (mine !== generation) return
      if (!test) {
        running = false
        return
      }
      view = test
    } catch (e) {
      error = (e as Error).message
      running = false
      return
    }
    timer = setTimeout(() => poll(mine), 100)
  }

  function stop() {
    generation++
    clearTimeout(timer)
    if (running) void local.micTestStop().catch(() => {})
    running = false
  }

  // Leaving the page (or tab) releases the mic.
  $effect(() => {
    if (!active) stop()
  })
  // A new mic or margin while running: start again with it.
  let lastKey = ''
  $effect(() => {
    const key = `${device}|${margin}`
    if (lastKey && key !== lastKey && running) void start()
    lastKey = key
  })
  onDestroy(stop)

  const db = (level: number | null | undefined) => (level == null ? '…' : `${levelDb(level)} dB`)
</script>

<Card title="Microphone test" description="Talk, pause and stay quiet to see what dictation hears. Nothing is recorded.">
  {#snippet aside()}
    {#if running}
      <button class="btn btn-sm" onclick={stop}><Icon name="stop" size={14} /> Stop</button>
    {:else}
      <button class="btn btn-sm" onclick={start}><Icon name="mic" size={14} /> Start test</button>
    {/if}
  {/snippet}
  {#if error}
    <div class="hint"><Icon name="alert" size={16} /> {error}</div>
  {/if}
  {#if running || view}
    {@const v = view}
    <div class="flex flex-col gap-3">
      <div class="relative h-4 overflow-hidden rounded-full bg-panel-2" aria-label="Microphone level">
        <div
          class={['absolute inset-y-0 left-0 rounded-full transition-[width] duration-75', v?.speech ? 'bg-me' : 'bg-faint']}
          style:width={`${levelPct(v?.level ?? 0)}%`}
        ></div>
        {#if v?.floor != null}
          <div class="absolute inset-y-0 w-0.5 bg-muted" style:left={`${levelPct(v.floor)}%`} title="Noise floor"></div>
        {/if}
        {#if v}
          <div class="absolute inset-y-0 w-0.5 bg-accent" style:left={`${levelPct(v.threshold)}%`} title="Speech threshold"></div>
        {/if}
      </div>
      <div class="flex flex-wrap items-center gap-x-5 gap-y-1 text-[12.5px] text-muted">
        <span
          class={['rounded-full px-2 py-0.5 text-[11.5px] font-medium', v?.speech ? 'bg-me-soft text-me' : 'bg-panel-2 text-muted']}
          >{v?.speech ? 'Speech' : 'Silence'}</span
        >
        <span>Level <span class="font-mono text-fg">{db(v?.level)}</span></span>
        <span class="flex items-center gap-1.5"
          ><span class="inline-block h-3 w-0.5 bg-muted"></span>Noise floor
          <span class="font-mono text-fg">{v?.floor == null ? 'measuring…' : db(v.floor)}</span></span
        >
        <span class="flex items-center gap-1.5"
          ><span class="inline-block h-3 w-0.5 bg-accent"></span>Threshold <span class="font-mono text-fg">{db(v?.threshold)}</span></span
        >
        <span
          >Phrases <span class="font-mono text-fg">{v?.phrases ?? 0}</span>{#if v?.last_phrase_secs != null}
            · last {v.last_phrase_secs.toFixed(1)} s{/if}</span
        >
      </div>
      <div class="hint hint-neutral">
        <span
          >The threshold is the room's noise floor plus the speech margin. If noise turns the bar green, raise the margin; if
          quiet words stay grey, lower it. A phrase ends after 0.6 s of silence.</span
        >
      </div>
    </div>
  {/if}
</Card>
