<script lang="ts">
	import { Disc3, Users, Music2, ArrowUpRight } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import { getLibraryArtistThumbsQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import type { LibraryStats } from '$lib/types';

	interface Props {
		stats: LibraryStats;
	}
	let { stats }: Props = $props();

	interface Art {
		key: string;
		mbid: string;
		remoteUrl: string | null;
		type: 'album' | 'artist';
	}

	const artistThumbsQuery = getLibraryArtistThumbsQuery();

	const albumArt = $derived<Art[]>(
		(stats.recently_added ?? []).slice(0, 9).map((a) => ({
			key: a.release_group_mbid,
			mbid: a.release_group_mbid,
			remoteUrl: a.cover_url,
			type: 'album'
		}))
	);
	const artistArt = $derived<Art[]>(
		(artistThumbsQuery.data?.items ?? [])
			.filter((a) => a.artist_mbid)
			.slice(0, 9)
			.map((a) => ({
				key: a.artist_mbid as string,
				mbid: a.artist_mbid as string,
				remoteUrl: null,
				type: 'artist'
			}))
	);
	// rotate the reused album pool so the tracks fan never mirrors the albums fan in a small library
	const trackArt = $derived<Art[]>(
		albumArt.length > 1
			? [
					...albumArt.slice(Math.ceil(albumArt.length / 2)),
					...albumArt.slice(0, Math.ceil(albumArt.length / 2))
				]
			: albumArt
	);

	const tiles = $derived([
		{
			href: '/library/albums',
			aria: 'Browse all albums',
			label: 'Albums',
			count: stats.total_albums,
			art: albumArt,
			base: 0,
			icon: Disc3,
			hover: 'hover:border-primary/30 focus-visible:ring-primary',
			arrowHover: 'group-hover:text-primary',
			blob: 'bg-primary/10'
		},
		{
			href: '/library/artists',
			aria: 'Browse all artists',
			label: 'Artists',
			count: stats.total_artists,
			art: artistArt,
			base: 3,
			icon: Users,
			hover: 'hover:border-accent/30 focus-visible:ring-accent',
			arrowHover: 'group-hover:text-accent',
			blob: 'bg-accent/10'
		},
		{
			href: '/library/tracks',
			aria: 'Browse all tracks',
			label: 'Tracks',
			count: stats.total_tracks,
			art: trackArt,
			base: 5,
			icon: Music2,
			hover: 'hover:border-primary/30 focus-visible:ring-primary',
			arrowHover: 'group-hover:text-primary',
			blob: 'bg-primary/10'
		}
	]);

	const WINDOW = 5;

	// one shared heartbeat advances every deck; per-deck offsets stagger them
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

	function visibleWindow(n: number): number {
		return Math.min(WINDOW, n);
	}

	function startIndex(base: number, n: number): number {
		// hold still unless the pool outnumbers the fan, else covers shuffle with nothing new to reveal
		if (n <= WINDOW) return 0;
		return (((cycle + base) % n) + n) % n;
	}

	function position(i: number, base: number, n: number): number {
		return (((i - startIndex(base, n)) % n) + n) % n;
	}

	// playing-card fan: outer cards rotate from a low pivot, dim and drop back; anything past the window parks off the nearest edge and fades
	function slotStyle(pos: number, win: number, n: number): string {
		if (pos >= win) {
			// park off-stage toward the side this card is heading, so it slides out past the spread and re-enters from the far edge rather than collapsing to centre
			const exiting = pos >= (win + n - 1) / 2;
			const edge = 56 * (exiting ? -1 : 1);
			return `transform:translate(calc(-50% + ${edge}px), calc(-50% + 10px)) scale(.72);opacity:0;z-index:0;`;
		}
		const off = pos - (win - 1) / 2;
		const a = Math.abs(off);
		const rot = off * 7;
		const tx = off * 14;
		const ty = a * 4;
		const scale = 1 - a * 0.05;
		const z = 40 - Math.round(a * 10);
		const opacity = 1 - a * 0.1;
		return `transform:translate(calc(-50% + ${tx}px), calc(-50% + ${ty}px)) rotate(${rot}deg) scale(${scale});opacity:${opacity};z-index:${z};`;
	}
</script>

{#snippet fan(art: Art[], base: number, FallbackIcon: typeof Disc3)}
	<div class="fan-deck relative h-full w-full">
		{#if art.length}
			{@const n = art.length}
			{@const win = visibleWindow(n)}
			{#each art as item, i (item.key)}
				<div
					class="fan-card absolute top-1/2 left-1/2 aspect-square w-14 overflow-hidden rounded-xl shadow-lg ring-1 ring-base-content/10"
					style={slotStyle(position(i, base, n), win, n)}
				>
					{#if item.type === 'album'}
						<AlbumImage
							mbid={item.mbid}
							remoteUrl={item.remoteUrl}
							alt=""
							size="full"
							rounded="none"
							className="h-full w-full object-cover"
						/>
					{:else}
						<ArtistImage
							mbid={item.mbid}
							remoteUrl={item.remoteUrl}
							alt=""
							size="full"
							rounded="none"
							className="h-full w-full object-cover"
						/>
					{/if}
				</div>
			{/each}
		{:else}
			<div class="absolute inset-0 flex items-center justify-center">
				<FallbackIcon class="h-10 w-10 text-base-content/10" strokeWidth={1.25} />
			</div>
		{/if}
	</div>
{/snippet}

<!-- isolate traps the fan cards' inline z-index (up to 40) in their own stacking
     context so they can't paint over the library search dropdown above them -->
<div class="isolate grid grid-cols-1 gap-4 sm:grid-cols-3">
	{#each tiles as tile (tile.href)}
		<a
			href={tile.href}
			class="hub-tile grain group relative flex h-32 items-center overflow-hidden rounded-2xl border border-base-content/8 bg-base-200/50 pr-3 pl-5 transition-all duration-300 hover:-translate-y-1 hover:bg-base-200 focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-offset-base-100 focus-visible:outline-none {tile.hover}"
			aria-label={tile.aria}
		>
			<div
				aria-hidden="true"
				class="pointer-events-none absolute -top-10 -right-6 h-32 w-32 rounded-full blur-3xl transition-transform duration-500 group-hover:scale-125 {tile.blob}"
			></div>
			<ArrowUpRight
				class="absolute top-3 right-3 z-50 h-4 w-4 text-base-content/25 transition-all duration-300 group-hover:translate-x-0.5 group-hover:-translate-y-0.5 {tile.arrowHover}"
			/>
			<div class="relative min-w-0 flex-1">
				<div class="font-display text-3xl leading-none font-bold tabular-nums sm:text-4xl">
					{tile.count.toLocaleString()}
				</div>
				<div
					class="mt-2 flex items-center gap-1.5 font-mono text-[0.68rem] font-bold tracking-[0.2em] text-base-content/50 uppercase"
				>
					<tile.icon class="h-3.5 w-3.5 text-accent" />
					{tile.label}
				</div>
			</div>
			<!-- hidden in the cramped sm-to-md band where three columns leave no room for the deck -->
			<div class="relative block h-full w-28 shrink-0 sm:hidden md:block">
				{@render fan(tile.art, tile.base, tile.icon)}
			</div>
		</a>
	{/each}
</div>

<style>
	.hub-tile {
		--grain-opacity: 0.05;
	}
	.fan-card {
		transition:
			transform 0.7s cubic-bezier(0.22, 1, 0.36, 1),
			opacity 0.7s ease;
		will-change: transform, opacity;
	}
	.group:hover .fan-deck {
		transform: scale(1.04);
		transition: transform 0.4s cubic-bezier(0.22, 1, 0.36, 1);
	}
	.fan-deck {
		transition: transform 0.4s cubic-bezier(0.22, 1, 0.36, 1);
	}
	@media (prefers-reduced-motion: reduce) {
		.fan-card,
		.group:hover .fan-deck {
			transition: none;
		}
	}
</style>
