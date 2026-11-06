<script lang="ts">
	import { ArrowRight, Disc3, Heart, Music, User } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import ArtistFan from '$lib/media/ArtistFan.svelte';
	import Card from '$lib/ui/Card.svelte';
	import FritEdge from '$lib/ui/FritEdge.svelte';
	import Illustration from '$lib/ui/Illustration.svelte';
	import {
		getLibraryArtistThumbsQuery,
		getLibraryRecentlyAddedQuery
	} from '$lib/features/library/queries/LibraryQueries.svelte';
	import type { components } from '$lib/api/v3/openapi';
	import { withBasePath } from '$lib/utils/basePath';

	type LibraryStats = components['schemas']['StatsView'];

	interface Props {
		stats: LibraryStats;
	}
	let { stats }: Props = $props();

	interface Art {
		key: string;
		albumId?: string;
		mbid: string;
		available: boolean;
	}

	const artistThumbsQuery = getLibraryArtistThumbsQuery();
	const recentlyAddedQuery = getLibraryRecentlyAddedQuery();

	const albumArt = $derived<Art[]>(
		(recentlyAddedQuery.data?.items ?? []).slice(0, 9).map((a) => ({
			key: a.id,
			albumId: a.id,
			mbid: a.release_group_mbid ?? '',
			available: a.cover_available
		}))
	);
	const artistArt = $derived<Art[]>(
		(artistThumbsQuery.data?.items ?? []).slice(0, 9).map((a) => ({
			key: a.id,
			mbid: a.artist_mbid ?? '',
			available: Boolean(a.artist_mbid)
		}))
	);

	const ALBUM_WINDOW = 5;

	// one shared heartbeat advances both decks; the offsets stagger them
	let cycle = $state(0);
	let reduced = $state(false);

	// mirrors actions/tilt.ts so flipping the preference mid-session stops the deck instead of hard-snapping
	$effect(() => {
		const mq = window.matchMedia('(prefers-reduced-motion: reduce)');
		reduced = mq.matches;
		const onChange = (e: MediaQueryListEvent) => (reduced = e.matches);
		mq.addEventListener('change', onChange);
		return () => mq.removeEventListener('change', onChange);
	});

	$effect(() => {
		if (reduced) return;
		const id = setInterval(() => (cycle += 1), 4200);
		return () => clearInterval(id);
	});

	// hold still unless the pool outnumbers the deck, else pictures shuffle with nothing new to show
	function visibleSlots(
		art: Art[],
		base: number,
		size: number
	): Array<{ item: Art; position: number }> {
		const n = art.length;
		const win = Math.min(size, n);
		const start = n <= size ? 0 : (((cycle + base) % n) + n) % n;
		return Array.from({ length: win }, (_, position) => ({
			item: art[(start + position) % n]!,
			position
		}));
	}

	// The covers fan to the left from the front one, each further back
	// turned a little more and tucked under; sizes are in the deck's height.
	function coverTransform(position: number, win: number): string {
		const depth = win - 1 - position;
		const x = -depth * 5;
		const y = -depth * 3;
		const turn = 8 - depth * 6.5;
		const scale = 1 - depth * 0.035;
		return `translate(calc(-50% + ${x}cqh), calc(-50% + ${y}cqh)) rotate(${turn}deg) scale(${scale})`;
	}
</script>

{#snippet tileText(count: number, label: string, Icon: typeof Disc3, go: string)}
	<span class="hub-tile__text">
		<span class="hub-tile__count">{count.toLocaleString()}</span>
		<span class="hub-tile__label"><Icon class="hub-tile__icon" aria-hidden="true" />{label}</span>
		<span class="hub-tile__go">{go}<ArrowRight class="size-4" aria-hidden="true" /></span>
	</span>
{/snippet}

<!-- isolate keeps the decks' z-index inside, so they never paint over the search dropdown above -->
<div class="hub-tiles isolate">
	<div class="hub-tiles__torn">
		<Card
			variant="torn"
			padding="none"
			href={withBasePath('/library/albums')}
			aria-label="Browse all albums"
			class="hub-tile"
		>
			{@render tileText(stats.total_albums, 'Albums', Disc3, 'Browse albums')}
			<span class="hub-deck hub-deck--albums" aria-hidden="true">
				{#each visibleSlots(albumArt, 0, ALBUM_WINDOW) as slot (slot.item.key)}
					<span
						class="hub-deck__cover"
						data-testid="library-fan-card"
						data-fan-key={slot.item.key}
						style:transform={coverTransform(slot.position, Math.min(ALBUM_WINDOW, albumArt.length))}
						style:z-index={slot.position + 1}
					>
						<AlbumImage
							albumId={slot.item.albumId}
							mbid={slot.item.mbid}
							available={slot.item.available}
							alt=""
							size="full"
							requestSize={250}
							testId="library-fan-image"
							rounded="none"
							className="h-full w-full object-cover"
						/>
					</span>
				{/each}
			</span>
		</Card>
	</div>

	<Card
		variant="bordered"
		padding="none"
		href={withBasePath('/library/artists')}
		aria-label="Browse all artists"
		class="hub-tile hub-tile--dark hub-tile--artists"
	>
		<FritEdge class="hub-tile__frit" />
		{@render tileText(stats.total_artists, 'Artists', User, 'Browse artists')}
		<ArtistFan
			artists={artistArt}
			class="hub-deck--artists"
			faceTestId="library-fan-card"
			imageTestId="library-fan-image"
		/>
	</Card>

	<div class="hub-tiles__torn">
		<Card
			variant="torn"
			padding="none"
			href={withBasePath('/library/tracks')}
			aria-label="Browse all tracks"
			class="hub-tile hub-tile--tracks"
		>
			{@render tileText(stats.total_tracks, 'Tracks', Music, 'Browse tracks')}
			<span class="hub-turntable" aria-hidden="true">
				<Illustration name="turntable" fit="cover" class="h-full w-full" />
				<span class="hub-turntable__stripes dn-decor dn-stripes"></span>
			</span>
		</Card>
	</div>

	<Card
		variant="bordered"
		padding="none"
		href={withBasePath('/library/albums?favorites=1')}
		aria-label="Browse your favourite albums"
		class="hub-tile hub-tile--dark hub-tile--favourites"
	>
		{@render tileText(stats.favorite_albums, 'Favourites', Heart, 'View favourites')}
		<Illustration name="drip-heart" class="hub-heart" />
	</Card>
</div>

<style>
	.hub-tiles {
		display: grid;
		grid-template-columns: 1.37fr 1.32fr 1.24fr 1fr;
		gap: 0.875rem;
	}

	/* the torn tiles take their edge and lift from a wrapper, as the mask
	   would cut a border or shadow of their own */
	.hub-tiles__torn {
		filter: drop-shadow(0 0 0.5px var(--dn-paper-border)) drop-shadow(0 8px 12px rgb(0 0 0 / 0.35));
	}

	.hub-tiles :global(.hub-tile) {
		position: relative;
		height: 14.75rem;
		overflow: hidden;
		container-type: size;
	}
	.hub-tiles :global(.hub-tile--dark) {
		border-radius: 0.75rem;
		background-color: var(--color-base-100);
	}
	.hub-tiles :global(.hub-tile--dark:hover) {
		border-color: var(--dn-edge);
	}

	.hub-tile__text {
		position: relative;
		z-index: 2;
		display: flex;
		flex: 1;
		flex-direction: column;
		align-items: flex-start;
		padding: 1.875rem 1.5rem 1.625rem;
	}

	.hub-tile__count {
		font-family: var(--font-display);
		font-size: 5rem;
		line-height: 0.9;
		letter-spacing: -0.005em;
		font-variant-numeric: tabular-nums;
		font-synthesis: none;
	}
	:global(.hub-tile--artists) .hub-tile__count {
		color: var(--dn-title);
	}

	.hub-tile__label {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		margin-top: 0.875rem;
		font-family: var(--font-mono);
		font-size: 0.9375rem;
		font-weight: 600;
		line-height: 1;
		letter-spacing: 0.28em;
		text-transform: uppercase;
	}
	.hub-tile__label :global(.hub-tile__icon) {
		width: 1.5rem;
		height: 1.5rem;
		flex: none;
	}
	:global(.hub-tile--artists) .hub-tile__label :global(.hub-tile__icon) {
		color: var(--dn-title);
	}

	.hub-tile__go {
		display: inline-flex;
		align-items: center;
		gap: 0.625rem;
		margin-top: auto;
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		text-transform: uppercase;
	}
	:global(.hub-tile):hover .hub-tile__go {
		text-decoration: underline;
		text-underline-offset: 4px;
	}

	/* the album covers: a fan of square sleeves at the right, never cropped */
	.hub-deck {
		position: absolute;
		inset: 0 0 0 auto;
		z-index: 1;
		width: 62%;
		pointer-events: none;
	}
	.hub-deck__cover {
		position: absolute;
		top: 54%;
		left: calc(100% - 35cqh);
		width: 60cqh;
		aspect-ratio: 1;
		overflow: hidden;
		border-radius: 3px;
		box-shadow:
			0 0 0 1px rgb(0 0 0 / 0.25),
			-6px 8px 18px -6px rgb(0 0 0 / 0.55);
		transition: transform 0.7s cubic-bezier(0.22, 1, 0.36, 1);
	}

	/* the artist pictures: one large round face at the bottom right, two behind */
	.hub-tiles :global(.hub-deck--artists) {
		position: absolute;
		inset: 0 0 0 auto;
		z-index: 1;
		width: 96cqh;
		pointer-events: none;
	}

	.hub-tiles :global(.hub-tile__frit) {
		z-index: 0;
	}

	/* the tracks tile: a turntable panel down its right side */
	.hub-turntable {
		position: absolute;
		inset: 0 0 0 auto;
		z-index: 1;
		width: 37%;
		background-color: var(--dn-vinyl);
		overflow: hidden;
	}
	.hub-turntable__stripes {
		position: absolute;
		right: 0;
		bottom: 0.75rem;
		width: 100%;
		height: 20%;
		font-size: 1rem;
		color: var(--dn-action);
	}

	/* the heart and its drips end above the label's row, so they never run
	   over the label however narrow the tile */
	.hub-tiles :global(.hub-heart) {
		position: absolute;
		top: 5%;
		right: 3%;
		z-index: 1;
		height: 44%;
		width: auto;
		aspect-ratio: 120 / 150;
	}

	@media (prefers-reduced-motion: reduce) {
		.hub-deck__cover {
			transition: none;
		}
	}

	@media (max-width: 1399px) {
		.hub-tiles {
			grid-template-columns: 1fr 1fr;
		}
	}

	/* Phone: one tile a row, each a short band */
	@media (max-width: 639px) {
		.hub-tiles {
			grid-template-columns: 1fr;
			gap: 0.75rem;
		}
		.hub-tiles :global(.hub-tile) {
			height: 8rem;
		}
		.hub-tile__text {
			padding: 0.875rem 1.125rem 0.875rem;
		}
		.hub-tile__count {
			font-size: 2.875rem;
		}
		.hub-tile__label {
			gap: 0.5rem;
			margin-top: 0.375rem;
			font-size: 0.8125rem;
		}
		.hub-tile__label :global(.hub-tile__icon) {
			width: 1.125rem;
			height: 1.125rem;
		}
		.hub-tile__go {
			font-size: 0.6875rem;
		}
		.hub-deck {
			width: 60%;
		}
		.hub-deck__cover {
			width: 82cqh;
			left: calc(100% - 46cqh);
		}
		.hub-tiles :global(.hub-deck--artists) {
			width: 110cqh;
			transform: scale(1.16);
			transform-origin: 100% 85%;
		}
		.hub-turntable {
			width: 50%;
		}
		/* a phone's tile is wide: the heart has the whole right side */
		.hub-tiles :global(.hub-heart) {
			top: 8%;
			height: 80%;
		}
	}
</style>
