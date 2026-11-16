<script lang="ts" generics="T extends string">
  let {
    options,
    value = $bindable(),
    label,
    small = false,
  }: { options: { value: T; label: string }[]; value: T; label: string; small?: boolean } = $props()
</script>

<div class="inline-flex gap-0.5 rounded-[9px] border border-line bg-panel-2 p-[3px]" role="radiogroup" aria-label={label}>
  {#each options as o (o.value)}
    <button
      type="button"
      role="radio"
      aria-checked={value === o.value}
      class={[
        'rounded-md font-medium transition-colors',
        small ? 'px-2.5 py-0.5 text-[12.5px]' : 'px-3 py-1',
        value === o.value ? 'bg-raised text-fg shadow-card' : 'text-muted hover:text-fg',
      ]}
      onclick={() => (value = o.value)}
    >
      {o.label}
    </button>
  {/each}
</div>
