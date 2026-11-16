<script lang="ts">
  import type { createTranscriber } from '../lib/transcriber.svelte'
  import Icon from './Icon.svelte'

  let { tx, small = false }: { tx: ReturnType<typeof createTranscriber>; small?: boolean } = $props()
  const iconSize = $derived(small ? 13 : 15)

  const p = $derived(tx.service.phase)

  function stop() {
    const busy = (tx.current.state?.jobs ?? []).some((j) => j.state === 'queued' || j.state === 'running')
    if (busy && !confirm('A transcription is still running. Stop the transcriber anyway?')) return
    void tx.stopService()
  }
</script>

{#snippet busy()}<span class="inline-flex animate-spin"><Icon name="spinner" size={iconSize} /></span>{/snippet}

{#if p === 'online' || p === 'stopping'}
  <button class={['btn', small && 'btn-sm']} onclick={stop} disabled={p === 'stopping'} title="Stop the transcriber">
    {#if p === 'stopping'}{@render busy()} Stopping…{:else}<span class="text-danger"><Icon name="stop" size={iconSize} filled /></span> Stop{/if}
  </button>
{:else}
  <button class={['btn btn-primary', small && 'btn-sm']} onclick={() => tx.startService()} disabled={p === 'starting'} title="Start the transcriber">
    {#if p === 'starting'}{@render busy()} Starting…{:else}<Icon name="play" size={iconSize} filled /> Start{/if}
  </button>
{/if}
