<script lang="ts">
  import { onMount } from 'svelte'
  import Recordings from './routes/Recordings.svelte'
  import Settings from './routes/Settings.svelte'
  import Icon from './components/Icon.svelte'
  import { createTranscriber } from './lib/transcriber.svelte'
  import { applyTheme, loadTheme, nextTheme, saveTheme, themeLabel } from './lib/theme'

  type Route = 'recordings' | 'settings'
  const routeOf = (): Route => (location.hash.startsWith('#/settings') ? 'settings' : 'recordings')
  let route = $state<Route>(routeOf())

  let theme = $state(loadTheme())
  $effect(() => applyTheme(theme))
  const cycleTheme = () => {
    theme = nextTheme(theme)
    saveTheme(theme)
  }

  const tx = createTranscriber()
  onMount(() => {
    tx.start()
    const onHash = () => (route = routeOf())
    window.addEventListener('hashchange', onHash)
    return () => {
      tx.stop()
      window.removeEventListener('hashchange', onHash)
    }
  })

  const status = $derived.by(() => {
    const t = tx.current
    const phase = tx.service.phase
    if (phase === 'starting') return { tone: 'busy', text: 'Starting transcriber…' }
    if (phase === 'stopping') return { tone: 'busy', text: 'Stopping transcriber…' }
    if (!t.online || !t.state) return { tone: 'off', text: 'Transcription offline' }
    const e = t.state.engine
    const label = (id: string | null) => t.state!.models.find((m) => m.id === id)?.label ?? id ?? ''
    if (e.loading) return { tone: 'busy', text: `Loading ${label(e.loading)}…` }
    if (e.error) return { tone: 'bad', text: 'Model failed to load' }
    if (e.active) return { tone: 'ok', text: `${label(e.active)} ready` }
    return { tone: 'off', text: 'No model loaded' }
  })
</script>

<div class="grid h-full grid-rows-[52px_1fr]">
  <header class="flex items-center gap-7 border-b border-line bg-panel pr-4 pl-[18px]">
    <div class="flex items-center gap-2.5 text-[15px] font-semibold tracking-tight">
      <!-- Same mark as the tray and exe icon (backend/recorder/src/icon.rs). -->
      <svg class="size-[22px] text-brand" viewBox="0 0 100 100" aria-hidden="true">
        <rect x="3" y="3" width="94" height="94" rx="24" fill="currentColor" />
        <g fill="#fff"><rect x="22.0" y="37" width="6.2" height="26" rx="3.1" /><rect x="34.5" y="27" width="6.2" height="46" rx="3.1" /><rect x="46.9" y="19" width="6.2" height="62" rx="3.1" /><rect x="59.3" y="27" width="6.2" height="46" rx="3.1" /><rect x="71.8" y="37" width="6.2" height="26" rx="3.1" /></g>
      </svg>AudCap
    </div>
    <nav class="flex flex-1 gap-1">
      {#each [['recordings', 'Recordings'], ['settings', 'Settings']] as [key, label] (key)}
        <a
          href={`#/${key}`}
          class={['rounded-lg px-3 py-1.5 font-medium no-underline', route === key ? 'bg-accent-soft text-fg' : 'text-muted hover:bg-panel-2 hover:text-fg']}
          >{label}</a
        >
      {/each}
    </nav>
    <a
      class="flex items-center gap-2 rounded-full border border-line px-3 py-1 text-[12.5px] whitespace-nowrap text-muted no-underline hover:border-line-strong"
      href="#/settings/transcription"
      title="Transcription settings"
    >
      <span
        class={[
          'size-2 rounded-full',
          status.tone === 'ok' && 'bg-ok',
          status.tone === 'busy' && 'animate-pulse bg-warn',
          status.tone === 'bad' && 'bg-danger',
          status.tone === 'off' && 'bg-faint',
        ]}
      ></span>{status.text}
    </a>
    <button type="button" class="icon-btn -ml-4" title={themeLabel(theme)} aria-label={themeLabel(theme)} onclick={cycleTheme}>
      <Icon name={theme === 'light' ? 'sun' : theme === 'dark' ? 'moon' : 'monitor'} />
    </button>
  </header>
  <!-- Both pages stay mounted: unsaved settings and the player survive switching tabs. -->
  <main class="relative min-h-0">
    <div class={['absolute inset-0', route !== 'recordings' && 'hidden']}><Recordings {tx} active={route === 'recordings'} /></div>
    <div class={['absolute inset-0', route !== 'settings' && 'hidden']}><Settings {tx} active={route === 'settings'} /></div>
  </main>
</div>
