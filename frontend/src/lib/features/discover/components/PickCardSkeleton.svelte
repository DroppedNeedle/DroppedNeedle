<!--
	The pick card while the week or the next extra loads: the torn sheet
	with the sleeve's square and the title, artist and reason as bars, in
	the card's own layout.
-->
<script lang="ts">
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		/** What is loading, for a screen reader. */
		label: string;
	}

	let { label }: Props = $props();
</script>

<Card variant="torn" padding="none" class="dn-pick-skel" aria-label={label} aria-busy="true">
	<div class="dn-pick-skel__ink" aria-hidden="true">
		<div class="dn-pick-skel__sleeve skeleton skeleton-shimmer"></div>
		<div class="dn-pick-skel__text">
			<div class="skeleton skeleton-shimmer h-3 w-2/5"></div>
			<div class="skeleton skeleton-shimmer h-10 w-4/5"></div>
			<div class="skeleton skeleton-shimmer h-4 w-1/2"></div>
			<div class="skeleton skeleton-shimmer mt-4 h-24 w-full"></div>
		</div>
	</div>
</Card>

<style>
	:global(.dn-pick-skel) {
		height: 100%;
		min-height: 34.5rem;
	}
	.dn-pick-skel__ink {
		display: grid;
		grid-template-columns: minmax(0, 22rem) minmax(0, 1fr);
		gap: 1.5rem;
		padding: 3.25rem 2.375rem 1.625rem;
	}
	.dn-pick-skel__sleeve {
		aspect-ratio: 1;
		border-radius: 0.375rem;
	}
	.dn-pick-skel__text {
		display: flex;
		flex-direction: column;
		gap: 0.625rem;
		padding-top: 1.25rem;
	}
	@media (max-width: 639px) {
		:global(.dn-pick-skel) {
			min-height: 0;
		}
		.dn-pick-skel__ink {
			grid-template-columns: minmax(0, 1fr);
			padding: 2.125rem 1rem 1rem;
		}
		.dn-pick-skel__text {
			padding-top: 0;
		}
	}
</style>
