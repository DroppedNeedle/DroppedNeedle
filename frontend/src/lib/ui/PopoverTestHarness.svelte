<script lang="ts">
	import Popover from './Popover.svelte';
</script>

<Popover label="Filter" title="Filters" count={1}>
	{#snippet children(close)}
		<input aria-label="Minimum" />
		<button type="button" onclick={close}>Done</button>
	{/snippet}
</Popover>
<button type="button">Next on the page</button>
