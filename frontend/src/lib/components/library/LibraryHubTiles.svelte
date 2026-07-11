<script lang="ts">
	import { resolve } from '$app/paths';
	import { Disc3, Users, Music2, ArrowUpRight } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import {
		getLibraryArtistThumbsQuery,
		getLibraryRecentlyAddedQuery
	} from '$lib/queries/library/LibraryQueries.svelte';
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
		available: boolean;
	}

	const artistThumbsQuery = getLibraryArtistThumbsQuery();
	const recentlyAddedQuery = getLibraryRecentlyAddedQuery();

	const albumArt = $derived<Art[]>(
		(recentlyAddedQuery.data?.items ?? []).slice(0, 9).map((a) => ({
			key: a.id,
			mbid: a.id,
			remoteUrl: null,
			type: 'album',
			available: a.cover_available
		}))
	);
	const artistArt = $derived<Art[]>(
		(artistThumbsQuery.data?.items ?? []).slice(0, 9).map((a) => ({
			key: a.id,
			mbid: a.id,
			remoteUrl: null,
			type: 'artist',
			available: a.musicbrainz_artist_id !== null
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

	const WINDOW = 5;

	// one shared heartbeat advances every deck; per-deck offsets stagger them
	let cycle = $state(0);
	let reduced = $state(false);
	let wide = $state(false);

	// mirrors actions/tilt.ts so flipping the preference mid-session stops the deck instead of hard-snapping
	$effect(() => {
		const mq = window.matchMedia('(prefers-reduced-motion: reduce)');
		const wideMq = window.matchMedia('(min-width: 640px)');
		reduced = mq.matches;
		wide = wideMq.matches;
		const onChange = (e: MediaQueryListEvent) => (reduced = e.matches);
		const onWideChange = (e: MediaQueryListEvent) => (wide = e.matches);
		mq.addEventListener('change', onChange);
		wideMq.addEventListener('change', onWideChange);
		return () => {
			mq.removeEventListener('change', onChange);
			wideMq.removeEventListener('change', onWideChange);
		};
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

	function visibleSlots(art: Art[], base: number): Array<{ item: Art; position: number }> {
		const n = art.length;
		const win = visibleWindow(n);
		const start = startIndex(base, n);
		return Array.from({ length: win }, (_, position) => ({
			item: art[(start + position) % n]!,
			position
		}));
	}

	// playing-card fan: outer cards rotate from a low pivot, dim and drop back; anything past the window parks off the nearest edge and fades
	function slotStyle(pos: number, win: number, hero: boolean, n: number): string {
		if (pos >= win) {
			// park off-stage toward the side this card is heading, so it slides out past the spread and re-enters from the far edge rather than collapsing to centre
			const exiting = pos >= (win + n - 1) / 2;
			const edge = (hero ? 128 : 64) * (exiting ? -1 : 1);
			const drop = hero ? 26 : 12;
			return `transform:translate(calc(-50% + ${edge}px), calc(-50% + ${drop}px)) scale(.72);opacity:0;z-index:0;`;
		}
		const off = pos - (win - 1) / 2;
		const a = Math.abs(off);
		const rot = off * (hero ? 9 : 7);
		const tx = off * (hero ? 52 : 23);
		const ty = a * (hero ? 10 : 6);
		const scale = 1 - a * 0.05;
		const z = 40 - Math.round(a * 10);
		const opacity = 1 - a * 0.1;
		return `transform:translate(calc(-50% + ${tx}px), calc(-50% + ${ty}px)) rotate(${rot}deg) scale(${scale});opacity:${opacity};z-index:${z};`;
	}
</script>

{#snippet fan(art: Art[], base: number, hero: boolean, FallbackIcon: typeof Disc3)}
	<div class="fan-deck relative h-full w-full">
		{#if art.length}
			{@const n = art.length}
			{@const win = visibleWindow(n)}
			{#each visibleSlots(art, base) as slot (slot.item.key)}
				<div
					data-testid="library-fan-card"
					data-fan-key={slot.item.key}
					data-request-size="250"
					class="fan-card absolute top-1/2 left-1/2 aspect-square overflow-hidden rounded-2xl shadow-xl ring-1 ring-base-content/10 {hero
						? 'w-40 sm:w-48'
						: 'w-20 sm:w-[5.5rem]'}"
					style={slotStyle(slot.position, win, hero, n)}
				>
					{#if slot.item.type === 'album'}
						<AlbumImage
							mbid={slot.item.mbid}
							source="local"
							available={slot.item.available}
							remoteUrl={slot.item.remoteUrl}
							alt=""
							size="full"
							requestSize={250}
							responsiveSizes={hero ? '(max-width: 639px) 160px, 192px' : undefined}
							testId="library-fan-image"
							rounded="none"
							className="h-full w-full object-cover"
						/>
					{:else}
						<ArtistImage
							mbid={slot.item.mbid}
							source="local"
							available={slot.item.available}
							remoteUrl={slot.item.remoteUrl}
							alt=""
							size="full"
							requestSize={250}
							responsiveSizes={hero ? '(max-width: 639px) 160px, 192px' : undefined}
							testId="library-fan-image"
							rounded="none"
							className="h-full w-full object-cover"
						/>
					{/if}
				</div>
			{/each}
		{:else}
			<div class="absolute inset-0 flex items-center justify-center">
				<FallbackIcon class="h-16 w-16 text-base-content/10" strokeWidth={1.25} />
			</div>
		{/if}
	</div>
{/snippet}

<!-- isolate traps the fan cards' inline z-index (up to 40) in their own stacking
     context so they can't paint over the library search dropdown above them -->
<div class="grid grid-cols-1 gap-4 sm:grid-cols-2 isolate">
	<a
		href={resolve('/library/albums')}
		class="group relative flex min-h-[27rem] flex-col overflow-hidden rounded-3xl border border-base-content/10 bg-gradient-to-br from-primary/20 via-primary/5 to-base-200/40 shadow-lg transition-all duration-300 hover:-translate-y-1 hover:border-primary/40 hover:shadow-2xl focus-visible:ring-2 focus-visible:ring-primary focus-visible:ring-offset-2 focus-visible:ring-offset-base-100 focus-visible:outline-none"
		aria-label="Browse all albums"
	>
		<div
			aria-hidden="true"
			class="pointer-events-none absolute -top-16 -right-12 h-52 w-52 rounded-full bg-primary/25 blur-3xl transition-transform duration-500 group-hover:scale-125"
		></div>
		<div
			aria-hidden="true"
			class="pointer-events-none absolute inset-0 opacity-[0.035]"
			style="background-image:url('data:image/svg+xml,<svg xmlns=%22http://www.w3.org/2000/svg%22 viewBox=%220 0 200 200%22><filter id=%22n%22><feTurbulence type=%22fractalNoise%22 baseFrequency=%220.9%22 numOctaves=%224%22 stitchTiles=%22stitch%22/></filter><rect width=%22100%25%22 height=%22100%25%22 filter=%22url(%23n)%22 opacity=%220.5%22/></svg>');background-size:200px;"
		></div>
		<ArrowUpRight
			class="absolute top-5 right-5 z-10 h-5 w-5 text-base-content/25 transition-all duration-300 group-hover:translate-x-0.5 group-hover:-translate-y-0.5 group-hover:text-primary"
		/>
		<div class="relative flex-1 px-6 pt-8 pb-2">
			{@render fan(albumArt, 0, true, Disc3)}
		</div>
		<div class="relative flex items-end justify-between px-6 pt-2 pb-6">
			<div>
				<div class="text-4xl leading-none font-black tabular-nums sm:text-5xl">
					{stats.total_albums.toLocaleString()}
				</div>
				<div
					class="mt-2 flex items-center gap-1.5 text-xs font-semibold tracking-[0.18em] text-base-content/55 uppercase"
				>
					<Disc3 class="h-3.5 w-3.5 text-primary" /> Albums
				</div>
			</div>
		</div>
	</a>

	<a
		href={resolve('/library/artists')}
		class="group relative flex min-h-[27rem] flex-col overflow-hidden rounded-3xl border border-base-content/10 bg-gradient-to-br from-accent/20 via-accent/5 to-base-200/40 shadow-lg transition-all duration-300 hover:-translate-y-1 hover:border-accent/40 hover:shadow-2xl focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-offset-2 focus-visible:ring-offset-base-100 focus-visible:outline-none"
		aria-label="Browse all artists"
	>
		<div
			aria-hidden="true"
			class="pointer-events-none absolute -top-16 -right-12 h-52 w-52 rounded-full bg-accent/25 blur-3xl transition-transform duration-500 group-hover:scale-125"
		></div>
		<div
			aria-hidden="true"
			class="pointer-events-none absolute inset-0 opacity-[0.035]"
			style="background-image:url('data:image/svg+xml,<svg xmlns=%22http://www.w3.org/2000/svg%22 viewBox=%220 0 200 200%22><filter id=%22n%22><feTurbulence type=%22fractalNoise%22 baseFrequency=%220.9%22 numOctaves=%224%22 stitchTiles=%22stitch%22/></filter><rect width=%22100%25%22 height=%22100%25%22 filter=%22url(%23n)%22 opacity=%220.5%22/></svg>');background-size:200px;"
		></div>
		<ArrowUpRight
			class="absolute top-5 right-5 z-10 h-5 w-5 text-base-content/25 transition-all duration-300 group-hover:translate-x-0.5 group-hover:-translate-y-0.5 group-hover:text-accent"
		/>
		<div class="relative flex-1 px-6 pt-8 pb-2">
			{@render fan(artistArt, 3, true, Users)}
		</div>
		<div class="relative flex items-end justify-between px-6 pt-2 pb-6">
			<div>
				<div class="text-4xl leading-none font-black tabular-nums sm:text-5xl">
					{stats.total_artists.toLocaleString()}
				</div>
				<div
					class="mt-2 flex items-center gap-1.5 text-xs font-semibold tracking-[0.18em] text-base-content/55 uppercase"
				>
					<Users class="h-3.5 w-3.5 text-accent" /> Artists
				</div>
			</div>
		</div>
	</a>

	<a
		href={resolve('/library/tracks')}
		class="group relative col-span-1 flex items-center gap-4 overflow-hidden rounded-3xl border border-base-content/10 bg-gradient-to-r from-info/15 via-info/5 to-base-200/40 px-6 py-5 shadow-lg transition-all duration-300 hover:-translate-y-1 hover:border-info/40 hover:shadow-2xl focus-visible:ring-2 focus-visible:ring-info focus-visible:ring-offset-2 focus-visible:ring-offset-base-100 focus-visible:outline-none sm:col-span-2 sm:gap-8 sm:px-8"
		aria-label="Browse all tracks"
	>
		<div
			aria-hidden="true"
			class="pointer-events-none absolute -top-12 right-1/3 h-44 w-44 rounded-full bg-info/20 blur-3xl transition-transform duration-500 group-hover:scale-125"
		></div>
		<div
			aria-hidden="true"
			class="pointer-events-none absolute inset-0 opacity-[0.035]"
			style="background-image:url('data:image/svg+xml,<svg xmlns=%22http://www.w3.org/2000/svg%22 viewBox=%220 0 200 200%22><filter id=%22n%22><feTurbulence type=%22fractalNoise%22 baseFrequency=%220.9%22 numOctaves=%224%22 stitchTiles=%22stitch%22/></filter><rect width=%22100%25%22 height=%22100%25%22 filter=%22url(%23n)%22 opacity=%220.5%22/></svg>');background-size:200px;"
		></div>
		{#if wide}
			<div class="relative h-28 w-52 shrink-0">
				{@render fan(trackArt, 5, false, Music2)}
			</div>
		{/if}
		<div class="relative min-w-0 flex-1">
			<div class="text-4xl leading-none font-black tabular-nums sm:text-5xl">
				{stats.total_tracks.toLocaleString()}
			</div>
			<div
				class="mt-2 flex items-center gap-1.5 text-xs font-semibold tracking-[0.18em] text-base-content/55 uppercase"
			>
				<Music2 class="h-3.5 w-3.5 text-info" /> Tracks
			</div>
		</div>
		<ArrowUpRight
			class="relative h-6 w-6 shrink-0 text-base-content/25 transition-all duration-300 group-hover:translate-x-0.5 group-hover:-translate-y-0.5 group-hover:text-info"
		/>
	</a>
</div>

<style>
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
