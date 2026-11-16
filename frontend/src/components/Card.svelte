<script lang="ts">
  import type { Snippet } from 'svelte'

  let { title, description = '', children, aside }: { title: string; description?: string; children: Snippet; aside?: Snippet } = $props()
</script>

<section class="rounded-2xl border border-line bg-panel shadow-card">
  <header class="flex items-start justify-between gap-4 border-b border-line px-5 py-4">
    <div>
      <h3 class="m-0 text-[15px] font-semibold">{title}</h3>
      {#if description}<p class="mt-0.5 text-[12.5px] text-muted">{description}</p>{/if}
    </div>
    {@render aside?.()}
  </header>
  <div class="flex flex-col gap-4 px-5 py-4">
    {@render children()}
  </div>
</section>
