<script lang="ts">
  let {
    checked = $bindable(),
    label,
    disabled = false,
    hideLabel = false,
  }: { checked: boolean; label: string; disabled?: boolean; hideLabel?: boolean } = $props()
</script>

<!-- `relative` keeps the absolutely positioned (sr-only) checkbox inside the label. Without it the
     checkbox sits outside the page's scroll area, and focusing it scrolls the whole window up,
     pushing the top bar out of reach. -->
<label class={['relative inline-flex items-center gap-3 select-none', disabled ? 'opacity-50' : 'cursor-pointer']}>
  <input type="checkbox" class="peer sr-only" bind:checked {disabled} />
  <span
    class="relative h-[22px] w-10 flex-none rounded-full bg-line-strong transition-colors peer-checked:bg-accent peer-focus-visible:outline-2 peer-focus-visible:outline-offset-2 peer-focus-visible:outline-accent after:absolute after:top-[3px] after:left-[3px] after:size-4 after:rounded-full after:bg-white after:shadow after:transition-transform peer-checked:after:translate-x-[18px]"
  ></span>
  <span class={hideLabel ? 'sr-only' : ''}>{label}</span>
</label>
