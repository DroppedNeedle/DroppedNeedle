<!-- Renders GapShelf with each cover's key as text, for its spec. -->
<script lang="ts">
	import GapShelf from './GapShelf.svelte';

	interface Props {
		items: { key: string; year?: number | null }[];
		year?: number | null;
		label: string;
	}

	let { items, year = null, label }: Props = $props();
</script>

<GapShelf {items} {year} {label}>
	{#snippet cover(item)}<span>{item.key}</span>{/snippet}
</GapShelf>
