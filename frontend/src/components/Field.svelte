<script lang="ts">
  import type { Snippet } from 'svelte'

  // A labelled row: label (and help) on the left, the control on the right.
  let { label, help = '', children }: { label: string; help?: string; children: Snippet } = $props()
</script>

<div class="grid grid-cols-1 items-center gap-x-6 gap-y-1.5 md:grid-cols-[minmax(180px,240px)_1fr]">
  <div>
    <div class="font-medium">{label}</div>
    {#if help}<div class="text-[12.5px] text-muted">{help}</div>{/if}
  </div>
  <div class="flex min-w-0 flex-wrap items-center gap-2">
    {@render children()}
  </div>
</div>
