<!-- The hub's discovery shelves, one after another. It has no heading of its own. -->
<script lang="ts">
	import { reveal } from '$lib/actions/reveal';
	import type { Snippet } from 'svelte';

	let { children }: { children: Snippet } = $props();
</script>

<div use:reveal class="space-y-10">
	{@render children()}
</div>
