<script lang="ts">
	/*
	 * Stations — the radio floor. One place for every way DroppedNeedle can
	 * program a continuous listen: Daily Mixes, personal radio stations, and
	 * genre stations. All data comes from the existing discover engine; every
	 * card starts real playback through the radio launcher.
	 */
	import PageHero from '$lib/ui/PageHero.svelte';
	import DailyMixCard from '$lib/components/DailyMixCard.svelte';
	import RadioCard from '$lib/components/RadioCard.svelte';
	import RadioPlayButton from '$lib/components/discover/RadioPlayButton.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import CarouselSkeleton from '$lib/components/CarouselSkeleton.svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import { getDiscoverQuery } from '$lib/queries/discover/DiscoverQuery.svelte';
	import type { HomeAlbum, HomeGenre } from '$lib/types';
	import { RadioTower, Sparkles, Radio, Music4 } from 'lucide-svelte';

	const discoverQuery = getDiscoverQuery();
	const data = $derived(discoverQuery.data ?? null);
	const loading = $derived(discoverQuery.isLoading);

	const dailyMixes = $derived(data?.daily_mixes ?? []);
	const radioSections = $derived(data?.radio_sections ?? []);
	const genres = $derived(
		((data?.genre_list?.items ?? []) as HomeGenre[]).filter((g) => !!g.name).slice(0, 12)
	);
	// real artwork for the genre wall: representative artist per genre from the
	// discover payload (CDN portrait when allowed, covers-proxy mbid otherwise)
	const genreArtistImages = $derived(data?.genre_artist_images ?? {});
	const genreArtists = $derived(data?.genre_artists ?? {});

	// featured hero row: the first personal station gets the marquee treatment
	const featuredStation = $derived(radioSections[0] ?? null);
	const restStations = $derived(radioSections.slice(1));
	const featuredAlbums = $derived(
		featuredStation && featuredStation.type === 'albums'
			? (featuredStation.items as HomeAlbum[])
			: []
	);
	const featuredSeedType = $derived.by(() => {
		const t = featuredStation?.radio_seed_type;
		return t === 'genre'
			? ('genre' as const)
			: t === 'album'
				? ('album' as const)
				: ('artist' as const);
	});

	// deterministic per-genre tint: the wash UNDER each genre's artwork
	function genreHue(name: string): number {
		let h = 0;
		for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) % 360;
		return h;
	}
	const hasAnything = $derived(
		dailyMixes.length > 0 || radioSections.length > 0 || genres.length > 0
	);
</script>

<svelte:head>
	<title>Stations - DroppedNeedle</title>
</svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<PageHero
		title="Stations"
		subtitle="Continuous listening, programmed from your taste. Drop the needle and let it run."
		eyebrow="Always on"
		tint="var(--color-primary)"
		{loading}
		isUpdating={discoverQuery.isRefetching}
	>
		{#snippet icon()}
			<RadioTower class="h-7 w-7" />
		{/snippet}
	</PageHero>

	<div class="space-y-12 px-4 pb-12 sm:px-6 lg:px-8">
		{#if loading && !data}
			{#each Array(2) as _, i (`stations-skeleton-${i}`)}
				<section>
					<div class="skeleton skeleton-shimmer mb-4 h-6 w-44"></div>
					<CarouselSkeleton />
				</section>
			{/each}
		{:else if !hasAnything}
			<EmptyState
				icon={RadioTower}
				title="No stations yet"
				description="Stations are built from your listening history. Play some music, or connect a scrobbling service, and mixes will appear here."
				ctaLabel="Open Discovery"
				ctaHref="/discover"
			/>
		{:else}
			{#if dailyMixes.length > 0}
				<section aria-label="Daily mixes">
					<h2
						class="mb-4 flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
					>
						<Sparkles class="h-4 w-4 text-accent" />
						Daily mixes
					</h2>
					<div class="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
						{#each dailyMixes as mix, i (`${mix.title}-${i}`)}
							<DailyMixCard section={mix} />
						{/each}
					</div>
				</section>
			{/if}

			{#if radioSections.length > 0}
				<section aria-label="Radio stations">
					<h2
						class="mb-4 flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
					>
						<Radio class="h-4 w-4 text-accent" />
						Your stations
					</h2>

					{#if featuredStation}
						<div
							class="dn-station-hero relative overflow-hidden rounded-3xl border border-base-content/10"
						>
							<!-- blurred artwork backdrop -->
							<div class="dn-station-hero__backdrop" aria-hidden="true">
								<AlbumImage
									mbid={featuredAlbums[0]?.mbid || ''}
									customUrl={featuredAlbums[0]?.image_url || null}
									alt=""
									size="full"
									rounded="none"
									lazy={false}
									showPlaceholder={false}
									className="h-full w-full"
								/>
							</div>
							<div
								class="absolute inset-0 bg-gradient-to-r from-black/85 via-black/55 to-black/30"
							></div>
							<div
								class="absolute inset-0 bg-gradient-to-t from-black/60 via-transparent to-black/20"
							></div>

							<div
								class="relative flex flex-col gap-6 p-5 sm:flex-row sm:items-center sm:justify-between sm:p-8"
							>
								<div class="min-w-0">
									<p
										class="mb-3 w-fit rounded-full border border-white/15 bg-white/10 px-3 py-1 font-mono text-[0.6rem] font-bold uppercase tracking-[0.22em] text-white/80 backdrop-blur-sm"
									>
										Featured station
									</p>
									<h3
										class="hero-title font-display text-2xl font-bold tracking-tight text-white drop-shadow-md sm:text-4xl"
									>
										{featuredStation.title}
									</h3>
									{#if featuredAlbums.length > 0}
										<p
											class="mt-2 font-mono text-[0.62rem] font-bold uppercase tracking-[0.18em] text-white/55"
										>
											{featuredAlbums.length} album{featuredAlbums.length !== 1 ? 's' : ''} on rotation
										</p>
									{/if}
									<div class="mt-5">
										<RadioPlayButton
											seed={{
												seed_type: featuredSeedType,
												seed_id: featuredStation.radio_seed_id ?? ''
											}}
											size="md"
											label="Play station"
										/>
									</div>
								</div>

								{#if featuredAlbums.length > 1}
									<!-- fanned cover strip -->
									<div class="hidden shrink-0 items-center pr-4 sm:flex" aria-hidden="true">
										{#each featuredAlbums.slice(0, 3) as album, i (album.mbid ?? `${album.name}-${i}`)}
											<div class="dn-station-hero__cover" style="z-index: {3 - i};">
												<AlbumImage
													mbid={album.mbid || ''}
													customUrl={album.image_url || null}
													alt={album.name}
													size="full"
													rounded="none"
													className="h-full w-full"
												/>
											</div>
										{/each}
									</div>
								{/if}
							</div>
						</div>
					{/if}

					{#if restStations.length > 0}
						<div class="mt-4 grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
							{#each restStations as radio, i (`radio-${radio.radio_seed_id}-${i}`)}
								<RadioCard
									seedType={radio.radio_seed_type ?? 'artist'}
									seedId={radio.radio_seed_id ?? ''}
									initialSection={radio}
								/>
							{/each}
						</div>
					{/if}
				</section>
			{/if}

			{#if genres.length > 0}
				<section aria-label="Genre stations">
					<h2
						class="mb-4 flex items-center gap-2.5 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
					>
						<Music4 class="h-4 w-4 text-accent" />
						Genre stations
					</h2>
					<div class="grid grid-cols-2 gap-3 md:grid-cols-3 lg:grid-cols-4 lg:gap-4">
						{#each genres as genre (genre.name)}
							{@const hue = genreHue(genre.name)}
							{@const artistMbid = genreArtists[genre.name] ?? ''}
							{@const cdnUrl = genreArtistImages[genre.name] ?? null}
							<div
								class="dn-genre-station group relative overflow-hidden rounded-2xl border border-base-content/8 transition-all hover:-translate-y-0.5 hover:border-primary/30 hover:shadow-lg"
								style="--station-tint: oklch(0.6 0.12 {hue})"
							>
								<div class="aspect-[4/3]"></div>

								<!-- tint wash UNDER the artwork (also the no-art fallback) -->
								<div class="dn-genre-station__wash absolute inset-0" aria-hidden="true"></div>

								<!-- representative artist portrait -->
								{#if artistMbid || cdnUrl}
									<div class="absolute inset-0 opacity-90">
										<ArtistImage
											mbid={artistMbid}
											remoteUrl={cdnUrl}
											alt=""
											size="full"
											rounded="none"
											showPlaceholder={false}
											className="dn-genre-station__art h-full w-full"
										/>
									</div>
								{/if}

								<!-- legibility scrim -->
								<div
									class="absolute inset-0 bg-gradient-to-t from-black/75 via-black/25 to-black/10"
									aria-hidden="true"
								></div>

								<div class="absolute inset-0 flex flex-col justify-between p-3.5">
									<p
										class="font-mono text-[0.55rem] font-bold uppercase tracking-[0.2em] text-white/60"
									>
										Station
									</p>
									<div class="flex items-end justify-between gap-2">
										<a
											href="/genre?name={encodeURIComponent(genre.name)}"
											class="min-w-0 flex-1"
											aria-label="Open {genre.name} genre"
										>
											<p
												class="truncate font-display text-lg font-bold capitalize tracking-tight text-white drop-shadow-md sm:text-xl"
											>
												{genre.name}
											</p>
											{#if genre.artist_count}
												<p class="truncate text-xs text-white/60">
													{genre.artist_count} artists
												</p>
											{/if}
										</a>
										<RadioPlayButton
											seed={{ seed_type: 'genre', seed_id: genre.name }}
											size="sm"
											variant="primary"
											label=""
										/>
									</div>
								</div>
							</div>
						{/each}
					</div>
				</section>
			{/if}
		{/if}
	</div>
</div>

<style>
	/* -------- featured station hero -------- */
	.dn-station-hero {
		background: oklch(0.18 0.012 100);
	}
	.dn-station-hero__backdrop {
		position: absolute;
		inset: -12%;
		filter: blur(26px) saturate(1.05) brightness(0.75);
		transform: scale(1.12);
	}
	.dn-station-hero__cover {
		position: relative;
		width: 7.5rem;
		height: 7.5rem;
		overflow: hidden;
		border-radius: 0.75rem;
		border: 1px solid oklch(1 0 0 / 0.14);
		box-shadow: 0 18px 40px oklch(0 0 0 / 0.45);
		transition: transform 0.5s var(--ease-spring);
	}
	.dn-station-hero__cover:nth-child(1) {
		transform: rotate(-5deg) translateY(4px);
	}
	.dn-station-hero__cover:nth-child(2) {
		margin-left: -2rem;
		transform: rotate(2deg) translateY(-4px);
	}
	.dn-station-hero__cover:nth-child(3) {
		margin-left: -2rem;
		transform: rotate(7deg) translateY(6px);
	}
	.dn-station-hero:hover .dn-station-hero__cover:nth-child(1) {
		transform: rotate(-7deg) translateY(2px);
	}
	.dn-station-hero:hover .dn-station-hero__cover:nth-child(3) {
		transform: rotate(9deg) translateY(4px);
	}

	/* -------- genre station cards -------- */
	.dn-genre-station__wash {
		background:
			radial-gradient(
				circle at 20% 0%,
				oklch(from var(--station-tint) calc(l + 0.12) c h / 0.85),
				transparent 70%
			),
			linear-gradient(
				150deg,
				oklch(from var(--station-tint) l c h / 0.9),
				oklch(from var(--station-tint) calc(l - 0.28) calc(c - 0.04) h)
			);
	}
	/* the artwork sits ON the wash: kill BaseImage's opaque backing so the tint
	   shows through while the portrait loads (or when a genre has no artwork) */
	.dn-genre-station :global(.dn-genre-station__art) {
		background-color: transparent;
	}

	@media (prefers-reduced-motion: reduce) {
		.dn-genre-station,
		.dn-station-hero__cover {
			transition: none !important;
		}
	}
</style>
