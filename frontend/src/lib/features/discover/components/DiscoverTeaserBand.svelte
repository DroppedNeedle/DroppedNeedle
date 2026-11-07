<!--
	Home's way into Discover: one link band with the compass ring (or the
	server's artist picks inside it), the Anton call and the queue's pitch.
	A stacked card with a chevron on phones.
-->
<script lang="ts">
	import { ArrowRight, ChevronRight, Compass } from 'lucide-svelte';
	import ArtistFan from '$lib/media/ArtistFan.svelte';
	import { createDeckBeat } from '$lib/media/deckBeat.svelte';
	import { getDiscoverQueueStatusQuery } from '$lib/features/discover/queries/DiscoverQueueQueries';
	import { withBasePath } from '$lib/utils/basePath';
	import type { DiscoverPreview } from '$lib/features/discover/queries/types';
	import FritEdge from '$lib/ui/FritEdge.svelte';

	interface Props {
		preview: DiscoverPreview | null;
	}

	let { preview }: Props = $props();

	// the queue count makes the pitch concrete ("12 albums waiting")
	const queueStatus = getDiscoverQueueStatusQuery();
	const queueCount = $derived(
		queueStatus.data?.status === 'ready' ? (queueStatus.data.item_count ?? null) : null
	);

	const beat = createDeckBeat();
	const fanArtists = $derived(
		(preview?.items ?? []).slice(0, 4).map((artist, i) => ({
			key: artist.musicbrainz_id ?? `${artist.name}-${i}`,
			mbid: artist.musicbrainz_id ?? '',
			available: Boolean(artist.musicbrainz_id)
		}))
	);
	const headline = $derived(
		queueCount
			? `${queueCount} fresh albums are waiting in your Discover queue`
			: preview?.seed_artist
				? `New music picked because you listen to ${preview.seed_artist}`
				: 'Fresh recommendations are waiting for you'
	);
</script>

<a href={withBasePath('/discover')} class="band group">
	<FritEdge place="top-left" class="band-frit" />
	<FritEdge place="top-right" class="band-frit" />

	<div class="relative flex items-center gap-4 sm:gap-6">
		<div class="ring" aria-hidden="true">
			{#if fanArtists.length > 0}
				<ArtistFan artists={fanArtists} cycle={beat.cycle} class="ring-fan" />
			{:else}
				<Compass class="size-7 sm:size-8" />
			{/if}
		</div>

		<div class="min-w-0 flex-1 md:flex md:items-center md:gap-6">
			<h2 class="font-display text-3xl leading-none text-title md:shrink-0 lg:text-4xl">
				Picked for you
			</h2>
			<div class="pitch mt-1.5 md:mt-0 md:pl-6">
				<p class="text-sm font-semibold">{headline}</p>
				<p class="mt-0.5 hidden text-xs text-muted sm:block">
					Sample albums, spin a station, and grab what you love.
				</p>
			</div>
		</div>

		<span class="open" aria-hidden="true">
			Open Discover
			<ArrowRight
				class="size-4 motion-safe:transition-transform motion-safe:group-hover:translate-x-0.5"
			/>
		</span>
		<ChevronRight class="size-6 shrink-0 text-title sm:hidden" aria-hidden="true" />
	</div>
</a>

<style>
	.band {
		position: relative;
		display: block;
		overflow: hidden;
		border: 2px solid var(--dn-action-edge);
		border-radius: var(--radius-box);
		background: var(--color-base-100);
		padding: 1rem 1.25rem;
		transition: background-color 150ms;
	}
	.band:hover {
		background: var(--color-base-200);
	}
	@media (min-width: 640px) {
		.band {
			padding: 1.25rem 2rem;
		}
	}

	/* the frit sits in the band's two top corners, kept small and off the words */
	.band :global(.dn-frit.band-frit) {
		width: 4rem;
		height: 100%;
	}
	@media (min-width: 640px) {
		.band :global(.dn-frit.band-frit) {
			width: 7rem;
		}
	}

	.ring {
		position: relative;
		display: grid;
		place-items: center;
		width: 3.5rem;
		height: 3.5rem;
		flex-shrink: 0;
		border: 2px solid var(--dn-title);
		border-radius: 9999px;
		background: var(--color-base-100);
		color: var(--dn-title);
		container-type: size;
		overflow: hidden;
	}

	/* the fan is laid out for a wider box: shrunk and moved up and left, its
	   front face sits whole inside the ring */
	.ring :global(.ring-fan) {
		position: absolute;
		inset: 0;
		transform: translate(-6%, -5%) scale(0.9);
	}
	@media (min-width: 640px) {
		.ring {
			width: 4rem;
			height: 4rem;
		}
	}

	@media (min-width: 768px) {
		.pitch {
			border-left: 1px solid var(--dn-line);
		}
	}

	.open {
		display: none;
		flex-shrink: 0;
		align-items: center;
		gap: 0.375rem;
		border: 1px solid var(--dn-action-edge);
		border-radius: var(--radius-field);
		background: var(--dn-action);
		color: var(--dn-action-content);
		padding: 0.5rem 0.875rem;
		font-size: 0.8125rem;
		font-weight: 700;
		letter-spacing: 0.04em;
		text-transform: uppercase;
	}
	@media (min-width: 640px) {
		.open {
			display: inline-flex;
		}
	}
</style>
