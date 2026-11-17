<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		id: string;
		children: Snippet;
	}

	let { id, children }: Props = $props();
</script>

<div class="home-widget" data-widget={id}>
	{@render children()}
</div>

<style>
	.home-widget {
		height: 100%;
		min-height: 0;
	}
</style>
