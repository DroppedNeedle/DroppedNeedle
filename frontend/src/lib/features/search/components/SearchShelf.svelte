<!--
	The tiles of a search: Library's list grid. On the All tab (`swipe`) a
	phone shows them as one sideways row, about three in view.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		label: string;
		swipe?: boolean;
		busy?: boolean;
		children: Snippet;
	}

	let { label, swipe = false, busy = false, children }: Props = $props();
</script>

<div
	class="dn-list-grid"
	class:search-shelf--swipe={swipe}
	role="group"
	aria-label={label}
	aria-busy={busy}
>
	{@render children()}
</div>

<style>
	@media (max-width: 639px) {
		.search-shelf--swipe {
			grid-template-columns: none;
			grid-auto-flow: column;
			grid-auto-columns: 6.75rem;
			margin-inline: -1rem;
			padding: 0 1rem 0.5rem;
			overflow-x: auto;
			scroll-padding-inline: 1rem;
			scroll-snap-type: x proximity;
			scrollbar-width: none;
		}
		.search-shelf--swipe > :global(*) {
			scroll-snap-align: start;
		}
	}
</style>
