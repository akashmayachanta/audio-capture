<script lang="ts">
  import type { createTranscriber } from '../lib/transcriber.svelte'

  let { tx, small = false }: { tx: ReturnType<typeof createTranscriber>; small?: boolean } = $props()

  const p = $derived(tx.service.phase)

  function stop() {
    const busy = (tx.current.state?.jobs ?? []).some((j) => j.state === 'queued' || j.state === 'running')
    if (busy && !confirm('A transcription is still running. Stop the transcriber anyway?')) return
    void tx.stopService()
  }
</script>

{#if p === 'online' || p === 'stopping'}
  <button class={['btn', small && 'btn-sm']} onclick={stop} disabled={p === 'stopping'}>{p === 'stopping' ? 'Stopping…' : 'Stop'}</button>
{:else}
  <button class={['btn btn-primary', small && 'btn-sm']} onclick={() => tx.startService()} disabled={p === 'starting'}
    >{p === 'starting' ? 'Starting…' : 'Start'}</button
  >
{/if}
