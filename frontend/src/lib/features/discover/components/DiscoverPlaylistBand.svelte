<!--
	The "Discover for a Playlist" band: a fan of tilted sleeves from covers
	already on the page, a slanted acid panel with the title, the line and
	the button that opens the playlist picker, and dots at the right end.
	The sleeves and dots are decoration.
-->
<script lang="ts">
	import { WandSparkles } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import FritEdge from '$lib/ui/FritEdge.svelte';

	interface Props {
		/** Release group ids of covers already shown on the page. */
		covers: string[];
		onopen: () => void;
	}

	let { covers, onopen }: Props = $props();

	const sleeves = $derived(covers.slice(0, 4));
</script>

<section class="dn-pband" aria-labelledby="playlist-band-title">
	{#if sleeves.length > 0}
		<div class="dn-decor dn-pband__sleeves" aria-hidden="true">
			{#each sleeves as mbid, i (mbid)}
				<span class="dn-pband__sleeve" style:--i={i}>
					<AlbumImage
						{mbid}
						alt=""
						size="full"
						requestSize={250}
						rounded="none"
						className="block aspect-square w-full object-cover"
					/>
				</span>
			{/each}
		</div>
	{/if}

	<h2 id="playlist-band-title" class="dn-pband__title">
		<span>Discover for</span>
		<span>a playlist</span>
	</h2>

	<div class="dn-pband__body">
		<p class="text-sm">Get album suggestions based on any playlist.</p>
		<Button variant="outline" size="display-sm" icon={WandSparkles} onclick={onopen}>
			Discover for a Playlist
		</Button>
	</div>

	<FritEdge class="dn-pband__frit" />
</section>

<style>
	.dn-pband {
		position: relative;
		display: flex;
		align-items: center;
		gap: 2rem;
		min-height: 10rem;
		padding: 1.25rem 2rem 1.25rem 1.5rem;
		overflow: hidden;
		border: 1px solid var(--dn-line);
		border-radius: var(--radius-box);
		background-color: var(--color-base-200);
	}

	/* covers as sleeves leaning on each other */
	.dn-pband__sleeves {
		position: relative;
		flex: none;
		width: 15rem;
		height: 7.5rem;
	}
	.dn-pband__sleeve {
		position: absolute;
		top: 50%;
		left: calc(var(--i) * 3.25rem);
		width: 6.5rem;
		padding: 3px;
		background-color: var(--dn-paper);
		border-radius: 3px;
		box-shadow: 4px 8px 18px -8px rgb(0 0 0 / 0.7);
		translate: 0 -50%;
		rotate: calc(-8deg + var(--i) * 5deg);
	}

	/* the slanted acid panel, its words upright */
	.dn-pband__title {
		position: relative;
		flex: none;
		display: flex;
		flex-direction: column;
		padding: 0.875rem 2.25rem 0.75rem 1.75rem;
		font-family: var(--font-display);
		font-weight: 400;
		font-size: 2.25rem;
		line-height: 0.95;
		text-transform: uppercase;
		color: var(--dn-action-content);
		font-synthesis: none;
		isolation: isolate;
	}
	.dn-pband__title::before {
		content: '';
		position: absolute;
		inset: 0;
		z-index: -1;
		background-color: var(--dn-action);
		box-shadow: inset 0 0 0 1.5px var(--dn-action-edge);
		transform: skewX(-10deg);
	}

	.dn-pband__body {
		position: relative;
		z-index: 1;
		display: flex;
		flex: 1 1 auto;
		flex-wrap: wrap;
		align-items: center;
		gap: 1rem 1.5rem;
		min-width: 0;
		padding-left: 1.75rem;
		border-left: 2px solid color-mix(in oklab, var(--dn-title) 70%, transparent);
	}
	.dn-pband__body p {
		max-width: 16rem;
		color: var(--color-base-content);
	}

	.dn-pband :global(.dn-pband__frit) {
		--frit-w: 12rem;
	}

	@media (max-width: 1279px) {
		.dn-pband__sleeves {
			width: 11rem;
		}
		.dn-pband__sleeve {
			left: calc(var(--i) * 2rem);
			width: 5.5rem;
		}
	}

	@media (max-width: 1023px) {
		.dn-pband__sleeves {
			display: none;
		}
	}

	@media (max-width: 639px) {
		.dn-pband {
			flex-direction: column;
			align-items: flex-start;
			gap: 1rem;
			padding: 1.25rem 1.25rem 1.5rem;
		}
		.dn-pband__title {
			font-size: 1.875rem;
			margin-left: 0.375rem;
		}
		.dn-pband__body {
			padding-left: 0;
			border-left: 0;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.dn-pband__sleeve {
			rotate: none;
		}
	}
</style>
