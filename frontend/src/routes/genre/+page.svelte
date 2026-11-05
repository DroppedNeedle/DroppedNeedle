<script lang="ts">
	import { page } from '$app/state';
	import GenreArtistCard from '$lib/features/genre/components/GenreArtistCard.svelte';
	import GenreAlbumCard from '$lib/features/genre/components/GenreAlbumCard.svelte';
	import GenreArtwork from '$lib/features/genre/components/GenreArtwork.svelte';
	import GenrePanel from '$lib/features/genre/components/GenrePanel.svelte';
	import GenreShelfGrid from '$lib/features/genre/components/GenreShelfGrid.svelte';
	import {
		albumHrefOrNull,
		artistHrefOrNull,
		localAlbumHref,
		localArtistHref
	} from '$lib/utils/entityRoutes';
	import { getGenreGradient } from '$lib/features/genre/components/genreGradient';
	import ServiceErrorState from '$lib/ui/ServiceErrorState.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import {
		getGenreAlbumPagesQuery,
		getGenreDetailQuery
	} from '$lib/features/genre/queries/genre.svelte';
	import { ArrowLeft, CircleAlert, Disc3, Library, Mic, Music2 } from 'lucide-svelte';
	import RadioPlayButton from '$lib/features/discover/components/RadioPlayButton.svelte';
	import type { RadioMode } from '$lib/features/player/playback/launchRadio';
	import type { SampleEntry } from '$lib/features/discover/state/deckSampler.svelte';
	import type { AlbumRef, ArtistRef } from '$lib/features/discover/queries/types';
	import BarcodeTicket from '$lib/ui/BarcodeTicket.svelte';
	import Button from '$lib/ui/Button.svelte';
	import VinylSleeve from '$lib/ui/VinylSleeve.svelte';
	import MediaTileSkeleton from '$lib/media/MediaTileSkeleton.svelte';

	let genreName = $derived(page.url.searchParams.get('name') || '');
	let loadAlbumPages = $state(false);
	let albumPagesGenre = $state('');
	const genreQuery = getGenreDetailQuery(() => genreName);
	const albumPagesQuery = getGenreAlbumPagesQuery(
		() => genreName,
		() => loadAlbumPages
	);
	let genreData = $derived(genreQuery.data?.pages[0] ?? null);
	let popularArtists = $derived(
		genreQuery.data?.pages.flatMap((item) => item.popular?.artists ?? []) ?? []
	);
	let popularAlbums = $derived([
		...(genreData?.popular?.albums ?? []),
		...(albumPagesQuery.data?.pages.flatMap((item) => item.popular?.albums ?? []) ?? [])
	]);
	let loading = $derived(Boolean(genreName) && genreQuery.isPending);
	let error = $derived(!genreName ? 'No genre specified' : '');
	let loadingMoreArtists = $derived(genreQuery.isFetchingNextPage);
	let loadingMoreAlbums = $derived(albumPagesQuery.isFetching);
	let hasMoreAlbums = $derived(
		loadAlbumPages
			? albumPagesQuery.isPending || albumPagesQuery.isError || albumPagesQuery.hasNextPage
			: Boolean(genreData?.popular?.has_more_albums)
	);

	$effect(() => {
		if (genreName !== albumPagesGenre) {
			albumPagesGenre = genreName;
			loadAlbumPages = false;
		}
	});

	function loadMoreAlbums() {
		if (!loadAlbumPages) {
			loadAlbumPages = true;
			return;
		}
		if (!albumPagesQuery.data) {
			void albumPagesQuery.refetch();
			return;
		}
		void albumPagesQuery.fetchNextPage();
	}

	const libraryArtists = $derived(genreData?.library?.artists ?? []);
	const libraryAlbums = $derived(genreData?.library?.albums ?? []);
	const hasLibraryContent = $derived(libraryArtists.length > 0 || libraryAlbums.length > 0);
	const libraryLine = $derived(
		`${genreData?.library?.artist_count ?? 0} artists · ${genreData?.library?.album_count ?? 0} albums`
	);
	const artistKey = (artist: ArtistRef) => artist.local_id || artist.musicbrainz_id || artist.name;
	const albumKey = (album: AlbumRef) => album.local_id || album.musicbrainz_id || album.title;

	// A long name steps down so it wraps to a few lines, never clips.
	const titleSize = $derived(
		genreName.length > 22 ? 'long' : genreName.length > 11 ? 'mid' : 'short'
	);

	// station mode: full tracks (library + YouTube), 30s previews, or library-only
	let radioMode = $state<RadioMode | 'previews'>('hybrid');
	const effectiveMode = $derived<RadioMode>(radioMode === 'previews' ? 'hybrid' : radioMode);
	const MODES: { value: RadioMode | 'previews'; label: string; hint: string }[] = [
		{ value: 'hybrid', label: 'Full tracks', hint: 'Your library plus streamed full tracks' },
		{ value: 'previews', label: 'Quick previews', hint: '30-second tastes - fast crate-digging' },
		{ value: 'library', label: 'My library', hint: 'Only music you own' }
	];

	// Quick-previews station: hear this genre's albums as 30s clips in the widget.
	// Popular (discovery) albums first, then library, deduped.
	const previewStation = $derived.by(() => {
		const seen: Record<string, true> = {};
		const entries: SampleEntry[] = [];
		const albums = [...popularAlbums, ...libraryAlbums];
		for (const album of albums) {
			if (!album.musicbrainz_id || seen[album.musicbrainz_id]) continue;
			seen[album.musicbrainz_id] = true;
			entries.push({
				key: album.musicbrainz_id,
				kind: 'album',
				artist: album.artist_name ?? '',
				title: album.title,
				albumMbid: album.musicbrainz_id,
				artistMbid: album.artist_id,
				coverUrl: album.cover_url
			});
		}
		return { title: genreName ? `${genreName} previews` : 'Genre previews', entries };
	});

	const skeletonItems = (count: number) => Array.from({ length: count }, (_, index) => index);
</script>

<svelte:head>
	<title>{genreName ? `${genreName}` : 'Genre'} - DroppedNeedle</title>
</svelte:head>

{#snippet libraryArtist(artist: ArtistRef)}
	<GenreArtistCard
		{artist}
		showLibraryBadge={true}
		href={artistHrefOrNull(artist.musicbrainz_id) ??
			(artist.local_id ? localArtistHref(artist.local_id) : null)}
	/>
{/snippet}

{#snippet libraryAlbum(album: AlbumRef)}
	<GenreAlbumCard
		{album}
		showLibraryBadge={true}
		href={albumHrefOrNull(album.musicbrainz_id) ??
			(album.local_id ? localAlbumHref(album.local_id) : null)}
	/>
{/snippet}

{#snippet popularArtist(artist: ArtistRef)}
	<GenreArtistCard {artist} href={artistHrefOrNull(artist.musicbrainz_id)} />
{/snippet}

{#snippet popularAlbum(album: AlbumRef)}
	<GenreAlbumCard {album} href={albumHrefOrNull(album.musicbrainz_id)} />
{/snippet}

{#snippet skeletonArtist(_index: number)}
	<MediaTileSkeleton kind="artist" />
{/snippet}

{#snippet skeletonAlbum(_index: number)}
	<MediaTileSkeleton />
{/snippet}

{#snippet empty(Icon: typeof Mic, message: string)}
	<div class="genre-empty">
		<Icon class="size-9" strokeWidth={1.5} aria-hidden="true" />
		<p>{message}</p>
	</div>
{/snippet}

<div class="genre-page">
	<section class="genre-hero" aria-labelledby="genre-title">
		<div class="genre-hero__back">
			<Button variant="caps" size="sm" icon={ArrowLeft} href={withBasePath('/')}>
				Back to home
			</Button>
		</div>
		<BarcodeTicket label="DN / CC 3.2" class="genre-hero__ticket" />

		<div class="genre-hero__grid">
			<VinylSleeve class="genre-hero__sleeve">
				<div class="genre-hero__art">
					<GenreArtwork
						artwork={genreData?.genre_artwork}
						gradientClass={getGenreGradient(genreName)}
					/>
					{#if !genreData?.genre_artwork.albums.length}
						<Music2 class="relative z-10 size-1/3 text-white/85" aria-hidden="true" />
					{/if}
				</div>
			</VinylSleeve>

			<div class="genre-hero__main">
				<h1 id="genre-title" class="genre-hero__title" data-size={titleSize}>
					{genreName || 'Genre'}
				</h1>
				{#if genreData}
					<p class="genre-hero__line">
						{#if hasLibraryContent}
							{libraryLine} in your library
						{:else}
							Explore popular {genreName} music
						{/if}
					</p>
				{/if}
				{#if genreName}
					<div class="genre-hero__actions">
						<RadioPlayButton
							seed={{ seed_type: 'genre', seed_id: genreName, mode: effectiveMode }}
							mode={effectiveMode}
							forcePreviews={radioMode === 'previews'}
							previewStation={radioMode === 'previews' ? previewStation : null}
							showShuffle={true}
							size="display"
							label="Play"
							className="genre-hero__play"
						/>
						<span class="dn-decor genre-hero__rule" aria-hidden="true"></span>
						<div class="genre-modes" role="radiogroup" aria-label="Station mode">
							{#each MODES as mode (mode.value)}
								<label class="genre-mode" title={mode.hint}>
									<input
										type="radio"
										class="radio radio-primary radio-sm"
										name="genre-station-mode"
										value={mode.value}
										bind:group={radioMode}
									/>
									{mode.label}
								</label>
							{/each}
						</div>
					</div>
				{/if}
			</div>
		</div>
	</section>

	<div class="genre-body">
		{#if loading}
			<GenrePanel icon={Library}>
				<GenreShelfGrid
					kind="artist"
					items={skeletonItems(12)}
					key={String}
					item={skeletonArtist}
					rows={2}
				/>
			</GenrePanel>
			<GenrePanel icon={Disc3}>
				<GenreShelfGrid
					kind="album"
					items={skeletonItems(6)}
					key={String}
					item={skeletonAlbum}
					rows={1}
				/>
			</GenrePanel>
		{:else if genreQuery.isError}
			<ServiceErrorState
				error={genreQuery.error}
				what="This genre"
				onretry={() => genreQuery.refetch()}
			/>
		{:else if error}
			{@render empty(CircleAlert, error)}
		{:else if genreData}
			{#key genreName}
				{#if hasLibraryContent}
					<GenrePanel title="From your library" sub={libraryLine} icon={Library} torn>
						<div
							class="genre-split"
							class:genre-split--two={libraryArtists.length > 0 && libraryAlbums.length > 0}
						>
							{#if libraryArtists.length > 0}
								<GenreShelfGrid
									kind="artist"
									title="Artists"
									items={libraryArtists}
									key={artistKey}
									item={libraryArtist}
									rows={2}
								/>
							{/if}
							{#if libraryAlbums.length > 0}
								<GenreShelfGrid
									kind="album"
									title="Albums"
									items={libraryAlbums}
									key={albumKey}
									item={libraryAlbum}
									rows={1}
									minShown={4}
								/>
							{/if}
						</div>
					</GenrePanel>
				{/if}

				<div class="genre-pair">
					<GenrePanel title="Top {genreName} artists" icon={Mic}>
						{#if popularArtists.length === 0}
							{@render empty(Mic, 'No artists found for this genre')}
						{:else}
							<GenreShelfGrid
								kind="artist"
								items={popularArtists}
								key={artistKey}
								item={popularArtist}
								rows={3}
								phoneRows={2}
								moreLabel="View more artists"
								hasMore={genreQuery.hasNextPage}
								loadingMore={loadingMoreArtists}
								onmore={() => genreQuery.fetchNextPage()}
							/>
						{/if}
					</GenrePanel>

					<GenrePanel title="Top {genreName} albums" icon={Disc3}>
						{#if popularAlbums.length === 0}
							{@render empty(Disc3, 'No albums found for this genre')}
						{:else}
							<GenreShelfGrid
								kind="album"
								items={popularAlbums}
								key={albumKey}
								item={popularAlbum}
								rows={3}
								phoneRows={2}
								moreLabel="View more albums"
								hasMore={hasMoreAlbums}
								loadingMore={loadingMoreAlbums}
								onmore={loadMoreAlbums}
							/>
						{/if}
					</GenrePanel>
				</div>
			{/key}
		{/if}
	</div>
</div>

<style>
	.genre-page {
		width: 100%;
		max-width: 90rem;
		margin-inline: auto;
		padding-bottom: 2.5rem;
	}

	/* The header: the art in its sleeve, the name, the line and the station. */
	.genre-hero {
		position: relative;
		overflow: clip;
		padding: 1rem 1rem 1.5rem;
	}

	.genre-hero__back {
		margin: 0 0 0.75rem -0.75rem;
	}

	.genre-hero :global(.genre-hero__ticket) {
		position: absolute;
		top: 0.75rem;
		right: 1rem;
		width: 6.5rem;
	}

	.genre-hero__grid {
		display: grid;
		gap: 1.5rem;
		align-items: center;
	}

	.genre-hero__grid > :global(.genre-hero__sleeve) {
		justify-self: center;
		width: min(100%, 15rem);
	}

	.genre-hero__art {
		position: relative;
		isolation: isolate;
		display: grid;
		aspect-ratio: 1;
		place-items: center;
		overflow: hidden;
	}

	.genre-hero__main {
		display: flex;
		min-width: 0;
		flex-direction: column;
		align-items: flex-start;
	}

	.genre-hero__title {
		max-width: 100%;
		font-family: var(--font-display);
		font-weight: 400;
		font-size: clamp(4rem, 1.5rem + 6.4vw, 8.5rem);
		line-height: 0.9;
		letter-spacing: 0.005em;
		text-transform: uppercase;
		color: var(--dn-title);
		overflow-wrap: anywhere;
		text-wrap: balance;
		font-synthesis: none;
	}
	.genre-hero__title[data-size='mid'] {
		font-size: clamp(2.75rem, 1rem + 4.2vw, 5.75rem);
		line-height: 0.92;
	}
	.genre-hero__title[data-size='long'] {
		font-size: clamp(2.25rem, 0.9rem + 2.8vw, 4rem);
		line-height: 0.96;
	}

	.genre-hero__line {
		margin-top: 0.875rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.2em;
		text-transform: uppercase;
	}

	.genre-hero__actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 1rem 1.5rem;
		margin-top: 1.5rem;
	}

	.genre-hero__actions :global(.genre-hero__play) {
		column-gap: 0.75rem;
	}

	.genre-hero__rule {
		display: none;
		align-self: stretch;
		width: 1px;
		margin-block: 0.375rem;
		background-color: var(--dn-line);
	}

	.genre-modes {
		display: flex;
		flex-wrap: wrap;
		gap: 0.75rem 1.5rem;
	}

	.genre-mode {
		display: inline-flex;
		min-height: 2rem;
		cursor: pointer;
		align-items: center;
		gap: 0.625rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		white-space: nowrap;
	}

	.genre-body {
		display: flex;
		flex-direction: column;
		gap: 1.5rem;
		padding-inline: 1rem;
	}

	.genre-split {
		display: grid;
		gap: 1.5rem;
	}

	.genre-pair {
		display: grid;
		align-items: start;
		gap: 1.5rem;
	}

	.genre-empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.75rem;
		padding-block: 3.5rem;
		text-align: center;
		color: var(--dn-muted);
	}

	@media (min-width: 640px) {
		.genre-hero {
			padding-inline: 1.5rem;
		}
		.genre-body {
			padding-inline: 1.5rem;
		}
		.genre-hero__rule {
			display: block;
		}
		.genre-hero :global(.genre-hero__ticket) {
			right: 1.5rem;
			width: 8.5rem;
		}
	}

	@media (min-width: 768px) {
		.genre-hero__grid {
			grid-template-columns: 15rem minmax(0, 1fr);
			gap: 2rem;
		}
		.genre-hero__grid > :global(.genre-hero__sleeve) {
			width: 100%;
		}
		.genre-hero__title {
			max-width: calc(100% - 5rem);
		}
	}

	@media (min-width: 1024px) {
		.genre-hero {
			padding: 1.25rem 2rem 2rem;
		}
		.genre-body {
			padding-inline: 2rem;
		}
		.genre-hero__grid {
			grid-template-columns: 19rem minmax(0, 1fr);
			gap: 2.5rem;
		}
		.genre-hero__title {
			max-width: calc(100% - 12rem);
		}
		.genre-hero :global(.genre-hero__ticket) {
			top: 3.5rem;
			right: 2rem;
			width: 12.5rem;
		}
	}

	@media (min-width: 1200px) {
		.genre-split--two {
			grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
			gap: 0;
		}
		.genre-split--two > :global(:last-child) {
			padding-left: 1.5rem;
			margin-left: 1.5rem;
			border-left: 1px solid var(--dn-line);
		}
		.genre-pair {
			grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
		}
	}

	@media (max-width: 479px) {
		.genre-hero__grid > :global(.genre-hero__sleeve) {
			width: min(100%, 13rem);
		}
		.genre-hero__line {
			font-size: 0.75rem;
			letter-spacing: 0.12em;
		}
		.genre-modes {
			gap: 0.5rem 1.25rem;
		}
		.genre-mode {
			font-size: 0.75rem;
			letter-spacing: 0.08em;
		}
	}
</style>
