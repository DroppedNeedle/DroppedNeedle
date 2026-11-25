<!--
	A pick's reason, on the small index card taped under its facts: the
	person's own artists it rests on, the line, and "Why this?", which opens
	the evidence. The card lifts while that link is hovered.
-->
<script lang="ts">
	import { ArrowRight } from 'lucide-svelte';
	import ArtistImage from '$lib/media/ArtistImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import type { SetPick } from '$lib/features/discover/queries/week.svelte';

	interface Props {
		reason: SetPick['reason'];
		onwhy: () => void;
	}

	let { reason, onwhy }: Props = $props();

	let lifted = $state(false);
</script>

<Card variant="index" size="sm" {lifted} class="dn-reason">
	<div class="dn-reason__head">
		<span class="dn-reason__caps">Why it's here</span>
		{#if reason.artists.length}
			<span class="dn-reason__seeds">
				{#each reason.artists as artist (artist.artist_mbid)}
					<ArtistImage
						mbid={artist.artist_mbid}
						alt={artist.name}
						size="xs"
						rounded="full"
						className="dn-reason__seed"
					/>
				{/each}
			</span>
		{/if}
	</div>
	<p class="dn-reason__line">{reason.line}</p>
	<Button
		variant="caps-link"
		size="sm"
		icon={ArrowRight}
		iconPosition="end"
		class="dn-reason__why"
		onclick={onwhy}
		onpointerenter={() => (lifted = true)}
		onpointerleave={() => (lifted = false)}
		onfocus={() => (lifted = true)}
		onblur={() => (lifted = false)}
	>
		Why this?
	</Button>
</Card>

<style>
	:global(.dn-reason) {
		margin-top: 1rem;
	}
	.dn-reason__head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.625rem;
		min-height: 1.75rem;
		padding-bottom: 0.4rem;
		margin-bottom: 0.55rem;
		border-bottom: 1.5px solid currentColor;
	}
	.dn-reason__caps {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.16em;
		text-transform: uppercase;
	}
	.dn-reason__seeds {
		display: flex;
	}
	.dn-reason__seeds :global(.dn-reason__seed) {
		width: 1.625rem;
		height: 1.625rem;
		margin-left: -0.45rem;
		box-shadow: 0 0 0 2px var(--dn-paper-white);
	}
	.dn-reason__line {
		margin: 0;
		font-size: 1.125rem;
		font-weight: 500;
		line-height: 1.34;
		text-wrap: pretty;
	}
	:global(.dn-reason__why) {
		align-self: flex-start;
		margin-top: 0.375rem;
		margin-left: -0.25rem;
	}
	@media (max-width: 639px) {
		:global(.dn-reason) {
			margin-top: 0.5rem;
		}
		.dn-reason__line {
			font-size: 0.97rem;
		}
		:global(.dn-reason__why) {
			margin-top: 0.125rem;
		}
	}
</style>
