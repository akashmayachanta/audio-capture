<script lang="ts">
  import { onMount } from 'svelte'
  import Card from '../components/Card.svelte'
  import Field from '../components/Field.svelte'
  import Icon from '../components/Icon.svelte'
  import Segmented from '../components/Segmented.svelte'
  import ServiceButton from '../components/ServiceButton.svelte'
  import Toggle from '../components/Toggle.svelte'
  import { local, transcriber, type CaptureMode, type Config, type Device, type PasteMode } from '../lib/api'
  import { joinTriggerApps, splitTriggerApps, type KnownApp } from '../lib/apps'
  import { rebase } from '../lib/config'
  import { keyOptions } from '../lib/keys'
  import { clampInt } from '../lib/numbers'
  import type { createTranscriber } from '../lib/transcriber.svelte'

  let { tx, active = true }: { tx: ReturnType<typeof createTranscriber>; active?: boolean } = $props()

  let saved = $state<Config | null>(null)
  let form = $state<Config | null>(null)
  let knownApps: string[] = []
  let known = $state<KnownApp[]>([])
  let custom = $state('')
  let mics = $state<Device[]>([])
  let speakers = $state<Device[]>([])
  let loadError = $state('')
  let saveError = $state('')
  let saving = $state(false)
  let justSaved = $state(false)
  let modelError = $state('')
  let picking = $state(false)
  type DictationKey = 'hold_key' | 'toggle_key'
  let detecting = $state<DictationKey | ''>('')
  let detectNote = $state<{ field: DictationKey; text: string } | null>(null)

  function adopt(c: Config) {
    saved = structuredClone(c)
    form = structuredClone(c)
    ;({ known, custom } = splitTriggerApps(c.trigger_apps, knownApps))
  }

  onMount(async () => {
    try {
      const r = await local.getConfig()
      knownApps = r.known_apps
      adopt(r.config)
    } catch (e) {
      loadError = (e as Error).message
    }
    try {
      ;({ mics, speakers } = await local.devices())
    } catch {
      // device list is optional; the selects still show the saved choice
    }
  })

  /**
   * Picks up what the tray menu (or an editor) wrote to config.toml since the form
   * loaded, keeping unsaved edits. Without it Save would write the old values back.
   */
  async function sync() {
    const fresh = (await local.getConfig()).config
    if (!saved || !candidate) return
    // $state.snapshot: structuredClone throws on reactive proxies.
    const merged = rebase($state.snapshot(saved), $state.snapshot(candidate), fresh)
    saved = fresh
    form = merged
    ;({ known, custom } = splitTriggerApps(merged.trigger_apps, knownApps))
  }

  onMount(() => {
    const onFocus = () => void sync().catch(() => {})
    window.addEventListener('focus', onFocus)
    return () => window.removeEventListener('focus', onFocus)
  })

  // Coming back to the tab.
  let wasActive = true // onMount does the first load
  $effect(() => {
    if (active && !wasActive) void sync().catch(() => {})
    wasActive = active
  })

  const candidate = $derived(
    form
      ? {
          ...form,
          trigger_apps: joinTriggerApps(known, custom),
          // Same ranges as config.rs `sanitized()`; out-of-range input would otherwise be a 400.
          bitrate_kbps: clampInt(form.bitrate_kbps, 6, 128),
          release_grace_secs: clampInt(form.release_grace_secs, 1, 600),
          min_duration_secs: clampInt(form.min_duration_secs, 0, 3600),
        }
      : null,
  )
  const dirty = $derived(!!candidate && JSON.stringify(candidate) !== JSON.stringify(saved))

  async function save() {
    if (!candidate) return
    saving = true
    saveError = ''
    try {
      await sync()
      const r = await local.putConfig(candidate)
      adopt(r.config)
      justSaved = true
      setTimeout(() => (justSaved = false), 2500)
    } catch (e) {
      saveError = (e as Error).message
    }
    saving = false
  }

  function discard() {
    // structuredClone throws on reactive proxies; snapshot first.
    if (saved) adopt($state.snapshot(saved))
    saveError = ''
  }

  async function browse() {
    if (picking) return
    picking = true
    saveError = ''
    try {
      const r = await local.pickFolder()
      if (r.path && form) form.output_dir = r.path
    } catch (e) {
      saveError = `Could not open the folder picker: ${(e as Error).message}`
    }
    picking = false
  }

  /** Binds the next key pressed (the window waits about 5 s) to a dictation gesture. */
  async function detect(field: DictationKey) {
    if (detecting) return
    detecting = field
    detectNote = null
    try {
      const { key } = await local.detectKey()
      if (key && form) form.dictation[field] = key.id
      detectNote = {
        field,
        text: key ? `Detected ${key.name}.` : 'No key detected. Some keyboards do not report keys such as Fn to Windows.',
      }
    } catch (e) {
      detectNote = { field, text: `Could not detect a key: ${(e as Error).message}` }
    }
    detecting = ''
  }

  async function loadModel(id: string) {
    modelError = ''
    try {
      await transcriber.load(id)
      await tx.poll()
    } catch (e) {
      modelError = (e as Error).message
    }
  }

  const missing = (id: string, list: Device[]) => id !== '' && !list.some((d) => d.id === id)
  const t = $derived(tx.current)
  // Loading another model frees the one a running transcription is using.
  const jobsRunning = $derived((t.state?.jobs ?? []).some((j) => j.state === 'queued' || j.state === 'running'))

  const DICTATION_KEYS: { field: DictationKey; label: string; help: string }[] = [
    { field: 'hold_key', label: 'Hold to talk', help: 'Hold the key, speak, release to paste.' },
    { field: 'toggle_key', label: 'Double-tap for hands-free', help: 'Double-tap to start, double-tap again to paste.' },
  ]

  const PASTE_MODES: { value: PasteMode; label: string }[] = [
    { value: 'live', label: 'While I speak' },
    { value: 'end', label: 'When I finish' },
  ]

  const MODES: { value: CaptureMode; label: string }[] = [
    { value: 'both', label: 'Mic + system audio' },
    { value: 'mic', label: 'Mic only' },
    { value: 'system', label: 'System audio only' },
  ]
</script>

<div class="h-full overflow-y-auto">
  <div class="mx-auto flex max-w-[860px] flex-col gap-5 px-6 pt-7 pb-28">
    <div>
      <h1 class="m-0 text-[21px] font-semibold tracking-tight">Settings</h1>
      <p class="mt-1 text-muted">Changes apply to the tray right after you save.</p>
    </div>

    {#if loadError}
      <div class="hint"><Icon name="alert" size={16} /> Could not read settings: {loadError}</div>
    {:else if form}
      <Card title="Capture" description="Your microphone is recorded as “Me”, what your speakers play as “Others”.">
        <Field label="Record">
          <Segmented label="Record" bind:value={form.capture_mode} options={MODES} />
        </Field>
        <Field label="Microphone">
          <select class="input w-full max-w-[420px]" bind:value={form.mic_device} disabled={form.capture_mode === 'system'}>
            <option value="">Windows default</option>
            {#each mics as d (d.id)}<option value={d.id}>{d.name}</option>{/each}
            {#if missing(form.mic_device, mics)}<option value={form.mic_device}>(device not connected)</option>{/if}
          </select>
        </Field>
        <Field label="Speakers" help="The device your meeting app plays to.">
          <select class="input w-full max-w-[420px]" bind:value={form.system_device} disabled={form.capture_mode === 'mic'}>
            <option value="">Windows default</option>
            {#each speakers as d (d.id)}<option value={d.id}>{d.name}</option>{/each}
            {#if missing(form.system_device, speakers)}<option value={form.system_device}>(device not connected)</option>{/if}
          </select>
        </Field>
      </Card>

      <Card title="Storage">
        <Field label="Recordings folder">
          <input class="input min-w-[260px] flex-1" bind:value={form.output_dir} spellcheck="false" />
          <button class="btn" onclick={browse} disabled={picking}><Icon name="folder" size={16} /> Browse…</button>
        </Field>
        <Field label="Quality" help="Bitrate per channel. 24 kbps is plenty for speech.">
          <input class="input w-24" type="number" min="6" max="128" bind:value={form.bitrate_kbps} />
          <span class="text-muted">kbps</span>
        </Field>
      </Card>

      <Card title="Auto-record" description="Start recording when a meeting app starts using the microphone.">
        {#snippet aside()}
          <Toggle label="Auto-record" hideLabel bind:checked={form!.auto_record} />
        {/snippet}
        <div class={['flex flex-col gap-4 transition-opacity', !form.auto_record && 'pointer-events-none opacity-50']}>
          <Field label="Apps">
            {#each known as k (k.name)}
              <button
                type="button"
                class={[
                  'inline-flex items-center gap-1.5 rounded-full border px-3 py-1 text-[13px] transition-colors',
                  k.on ? 'border-accent bg-accent-soft text-fg' : 'border-line-strong text-muted hover:border-faint hover:text-fg',
                ]}
                aria-pressed={k.on}
                onclick={() => (k.on = !k.on)}
              >
                {#if k.on}<Icon name="check" size={14} />{/if}{k.name}
              </button>
            {/each}
          </Field>
          <Field label="Other apps" help="Exe names, comma-separated.">
            <input class="input w-full max-w-[420px]" placeholder="Discord, Signal" bind:value={custom} spellcheck="false" />
          </Field>
          <Field label="Stop after" help="How long the mic must be free before the recording ends.">
            <input class="input w-24" type="number" min="1" max="600" bind:value={form.release_grace_secs} />
            <span class="text-muted">seconds</span>
          </Field>
          <Field label="Discard shorter than" help="Auto recordings shorter than this are deleted.">
            <input class="input w-24" type="number" min="0" max="3600" bind:value={form.min_duration_secs} />
            <span class="text-muted">seconds</span>
          </Field>
        </div>
      </Card>

      <Card title="Hotkeys" description="Global shortcuts, e.g. ctrl+alt+R.">
        {#each [['start_stop', 'Start / stop'], ['pause', 'Pause / resume'], ['split', 'Split into a new file'], ['marker', 'Add marker']] as const as [key, label] (key)}
          <Field {label}>
            <input class="input w-56 font-mono text-[13px]" placeholder="e.g. ctrl+alt+R" bind:value={form.hotkeys[key]} spellcheck="false" />
          </Field>
        {/each}
      </Card>

      <Card title="Dictation" description="Talk into any text field: your words are typed where the cursor is.">
        {#snippet aside()}
          <Toggle label="Dictation" hideLabel bind:checked={form!.dictation.enabled} />
        {/snippet}
        <div class={['flex flex-col gap-4 transition-opacity', !form.dictation.enabled && 'pointer-events-none opacity-50']}>
          {#each DICTATION_KEYS as k (k.field)}
            <Field label={k.label} help={k.help}>
              <select class="input w-56" bind:value={form.dictation[k.field]} disabled={!!detecting}>
                {#each keyOptions(form.dictation[k.field]) as o (o.id)}<option value={o.id}>{o.label}</option>{/each}
              </select>
              <button class="btn" onclick={() => detect(k.field)} disabled={!!detecting}
                >{detecting === k.field ? 'Press a key…' : 'Detect…'}</button
              >
              {#if detectNote?.field === k.field}<span class="basis-full text-[12.5px] text-muted">{detectNote.text}</span>{/if}
            </Field>
          {/each}
          <Field
            label="Paste the text"
            help="“While I speak” types each phrase after a short pause, in hands-free mode. Hold to talk always pastes on release."
          >
            <Segmented label="Paste the text" bind:value={form.dictation.paste_mode} options={PASTE_MODES} />
          </Field>
          <Toggle label="Keep the audio of each dictation" bind:checked={form.dictation.keep_audio} />
          <div class="hint hint-neutral">
            <span
              >Dictation needs the speech service (the transcriber) with a model loaded. If it is not running, the first
              dictation starts it. Esc cancels. Audio is kept in the “dictation” folder inside the recordings folder.</span
            >
          </div>
        </div>
      </Card>

      <Card title="Startup">
        <Toggle label="Start MeetRec when I sign in to Windows" bind:checked={form.start_with_windows} />
      </Card>
    {/if}

    <Card title="Transcription" description="Runs in the separate transcriber; one model is kept in memory.">
      {#snippet aside()}
        <div class="flex items-center gap-2.5">
          <span class={['rounded-full px-2.5 py-0.5 text-xs font-medium', t.online ? 'bg-ok/12 text-ok' : 'bg-panel-2 text-muted']}
            >{t.online ? 'Online' : 'Offline'}</span
          >
          <ServiceButton {tx} small />
        </div>
      {/snippet}
      {#if tx.service.failure}
        <div class="hint"><Icon name="alert" size={16} /> <span class="whitespace-pre-wrap">{tx.service.failure}</span></div>
      {/if}
      {#if !t.online}
        <div class="hint hint-neutral">
          <span
            >{tx.service.phase === 'starting'
              ? 'Starting the transcriber… The first start sets up its Python environment and can take a few minutes.'
              : 'Start the transcriber to load models and transcribe. Existing transcripts open without it. It keeps running after this window closes.'}</span
          >
        </div>
      {/if}
      {#if modelError}
        <div class="hint"><Icon name="alert" size={16} /> {modelError}</div>
      {/if}
      {#each t.state?.models ?? [] as m (m.id)}
        {@const e = t.state!.engine}
        <div class="flex items-start gap-4 rounded-xl border border-line p-4">
          <div class="min-w-0 flex-1">
            <div class="flex items-center gap-2 font-medium">
              {m.label}
              {#if e.loading === m.id}
                <span class="rounded-full bg-warn-soft px-2 py-0.5 text-[11.5px] text-warn">Loading…</span>
              {:else if e.active === m.id && e.loaded === m.id}
                <span class="rounded-full bg-ok/12 px-2 py-0.5 text-[11.5px] text-ok">Loaded</span>
              {:else if e.active === m.id && e.error}
                <span class="rounded-full bg-danger/12 px-2 py-0.5 text-[11.5px] text-danger" title={e.error}>Failed</span>
              {/if}
            </div>
            <p class="mt-1 text-[12.5px] text-muted">{m.description}</p>
            {#if e.active === m.id && e.error}<p class="mt-1 text-[12.5px] text-danger">{e.error}</p>{/if}
          </div>
          <button
            class="btn btn-sm"
            disabled={!t.online || !!e.loading || e.loaded === m.id || jobsRunning}
            title={jobsRunning ? 'Wait for the running transcription to finish' : undefined}
            onclick={() => loadModel(m.id)}>Load</button
          >
        </div>
      {/each}
    </Card>
  </div>
</div>

{#if dirty || saveError || justSaved}
  <div class="pointer-events-none fixed inset-x-0 bottom-5 flex justify-center px-4">
    <div class="pointer-events-auto flex items-center gap-3 rounded-2xl border border-line bg-raised py-2.5 pr-2.5 pl-4 shadow-xl">
      {#if saveError}
        <span class="flex items-center gap-1.5 text-[13px] text-danger"><Icon name="alert" size={16} /> {saveError}</span>
      {:else if dirty}
        <span class="text-[13px] text-muted">You have unsaved changes</span>
      {:else}
        <span class="flex items-center gap-1.5 text-[13px] text-ok"><Icon name="check" size={16} /> Saved</span>
      {/if}
      {#if dirty}
        <button class="btn" onclick={discard} disabled={saving}>Discard</button>
        <button class="btn btn-primary" onclick={save} disabled={saving}>{saving ? 'Saving…' : 'Save'}</button>
      {/if}
    </div>
  </div>
{/if}
