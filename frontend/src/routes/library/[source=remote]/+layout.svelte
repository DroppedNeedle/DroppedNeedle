<script lang="ts">
	import type { Snippet } from 'svelte';
	import type { LayoutData } from './$types';

	let { data, children }: { data: LayoutData; children: Snippet } = $props();
</script>

<!-- A page left for another server starts afresh: no sort, filter or page carries over. -->
{#key data.remote.source}
	{@render children()}
{/key}
