<script lang="ts">
	import { page } from '$app/state';
	import GenreArtistCard from '$lib/components/GenreArtistCard.svelte';
	import GenreAlbumCard from '$lib/components/GenreAlbumCard.svelte';
	import GenreArtwork from '$lib/components/GenreArtwork.svelte';
	import {
		albumHrefOrNull,
		artistHrefOrNull,
		localAlbumHref,
		localArtistHref
	} from '$lib/utils/entityRoutes';
	import { getGenreGradient } from '$lib/utils/genreGradient';
	import ServiceErrorState from '$lib/components/ServiceErrorState.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import {
		getGenreAlbumPagesQuery,
		getGenreDetailQuery
	} from '$lib/queries/genre/GenreQueries.svelte';
	import { ArrowLeft, BookOpen, Music2, CircleAlert, Mic, Disc3, ChevronDown } from 'lucide-svelte';
	import RadioPlayButton from '$lib/components/discover/RadioPlayButton.svelte';
	import type { RadioMode } from '$lib/player/launchRadio';
	import type { SampleEntry } from '$lib/stores/deckSampler.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

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

	const hasLibraryContent = $derived.by(() => {
		const data = genreData;
		return (data?.library?.artists?.length ?? 0) > 0 || (data?.library?.albums?.length ?? 0) > 0;
	});

	// station mode: full tracks (library + YouTube), 30s previews, or library-only
	let radioMode = $state<RadioMode | 'previews'>('hybrid');
	const effectiveMode = $derived<RadioMode>(radioMode === 'previews' ? 'hybrid' : radioMode);

	// Quick-previews station: hear this genre's albums as 30s clips in the widget.
	// Popular (discovery) albums first, then library, deduped.
	const previewStation = $derived.by(() => {
		const seen: Record<string, true> = {};
		const entries: SampleEntry[] = [];
		const albums = [...popularAlbums, ...(genreData?.library?.albums ?? [])];
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
</script>

<svelte:head>
	<title>{genreName ? `${genreName}` : 'Genre'} - DroppedNeedle</title>
</svelte:head>

<div class="min-h-screen bg-base-100 relative overflow-hidden">
	<div class="container relative z-[1] mx-auto max-w-7xl p-4">
		<header class="mb-10 pt-2">
			<Button
				variant="ghost"
				size="sm"
				icon={ArrowLeft}
				href={withBasePath('/')}
				class="mb-6 -ml-2"
			>
				Back
			</Button>
			<div class="flex items-center gap-5">
				<div
					class="relative isolate w-20 h-20 overflow-hidden rounded-2xl flex items-center justify-center shrink-0"
				>
					<GenreArtwork
						artwork={genreData?.genre_artwork}
						gradientClass={getGenreGradient(genreName)}
					/>
					{#if !genreData?.genre_artwork.albums.length}
						<Music2 class="relative z-10 h-10 w-10 text-white/80" />
					{/if}
				</div>
				<div>
					<h1 class="text-4xl sm:text-5xl font-bold capitalize tracking-tight">
						{genreName || 'Genre'}
					</h1>
					{#if genreData}
						<p class="text-base-content/50 mt-2 text-sm sm:text-base">
							{#if hasLibraryContent}
								{genreData.library?.artist_count ?? 0} artists · {genreData.library?.album_count ??
									0} albums in your library
							{:else}
								Explore popular {genreName} music
							{/if}
						</p>
					{/if}
				</div>
			</div>
			{#if genreName}
				<div class="mt-6 flex flex-wrap items-center gap-3">
					<RadioPlayButton
						seed={{ seed_type: 'genre', seed_id: genreName, mode: effectiveMode }}
						mode={effectiveMode}
						forcePreviews={radioMode === 'previews'}
						previewStation={radioMode === 'previews' ? previewStation : null}
						showShuffle={true}
						size="md"
						label="Play"
					/>
					<div class="join" role="radiogroup" aria-label="Station mode">
						<Button
							variant={radioMode === 'hybrid' ? 'neutral' : 'ghost'}
							size="xs"
							active={radioMode === 'hybrid'}
							class="join-item"
							role="radio"
							aria-checked={radioMode === 'hybrid'}
							onclick={() => (radioMode = 'hybrid')}
							title="Your library plus streamed full tracks"
						>
							Full tracks
						</Button>
						<Button
							variant={radioMode === 'previews' ? 'neutral' : 'ghost'}
							size="xs"
							active={radioMode === 'previews'}
							class="join-item"
							role="radio"
							aria-checked={radioMode === 'previews'}
							onclick={() => (radioMode = 'previews')}
							title="30-second tastes - fast crate-digging"
						>
							Quick previews
						</Button>
						<Button
							variant={radioMode === 'library' ? 'neutral' : 'ghost'}
							size="xs"
							active={radioMode === 'library'}
							class="join-item"
							role="radio"
							aria-checked={radioMode === 'library'}
							onclick={() => (radioMode = 'library')}
							title="Only music you own"
						>
							My library
						</Button>
					</div>
				</div>
			{/if}
		</header>

		{#if loading}
			<section class="mb-12" aria-label="Loading">
				<div class="flex items-center gap-3 mb-6">
					<div class="skeleton w-10 h-10 rounded-xl"></div>
					<div>
						<div class="skeleton h-6 w-48 mb-2"></div>
						<div class="skeleton h-4 w-32"></div>
					</div>
				</div>
				<div
					class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
				>
					{#each Array(12) as _, i (`genre-artist-skeleton-${i}`)}
						<Card padding="none">
							<div class="skeleton aspect-square rounded-t-2xl"></div>
							<div class="px-3 pb-3 pt-1">
								<div class="skeleton h-4 w-3/4 mb-2"></div>
								<div class="skeleton h-3 w-1/2"></div>
							</div>
						</Card>
					{/each}
				</div>
			</section>

			<section class="mb-12" aria-label="Loading">
				<div class="flex items-center gap-3 mb-6">
					<div class="skeleton w-10 h-10 rounded-xl"></div>
					<div>
						<div class="skeleton h-6 w-40 mb-2"></div>
						<div class="skeleton h-4 w-28"></div>
					</div>
				</div>
				<div
					class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
				>
					{#each Array(6) as _, i (`genre-album-skeleton-${i}`)}
						<Card padding="none">
							<div class="skeleton aspect-square rounded-t-2xl"></div>
							<div class="px-3 pb-3 pt-1">
								<div class="skeleton h-4 w-3/4 mb-2"></div>
								<div class="skeleton h-3 w-1/2"></div>
							</div>
						</Card>
					{/each}
				</div>
			</section>
		{:else if genreQuery.isError}
			<ServiceErrorState
				error={genreQuery.error}
				what="This genre"
				onretry={() => genreQuery.refetch()}
			/>
		{:else if error}
			<div class="flex flex-col items-center justify-center py-24">
				<CircleAlert class="h-12 w-12 text-base-content/40 mb-4" strokeWidth={1.5} />
				<p class="text-base-content/70 text-lg">{error}</p>
			</div>
		{:else if genreData}
			{#if hasLibraryContent}
				<section class="mb-12" aria-label="From Your Library">
					<div class="flex items-center gap-3 mb-6">
						<div
							class="w-10 h-10 rounded-xl bg-success/20 flex items-center justify-center text-success"
						>
							<BookOpen class="w-5 h-5" />
						</div>
						<div>
							<h2 class="text-2xl font-bold">From Your Library</h2>
							<p class="text-sm text-base-content/50">
								{genreData.library?.artist_count ?? 0} artists · {genreData.library?.album_count ??
									0} albums
							</p>
						</div>
					</div>

					{#if (genreData.library?.artists?.length ?? 0) > 0}
						<h3 class="text-lg font-semibold mb-4 text-base-content/70">Artists</h3>
						<div
							class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4 mb-8"
						>
							{#each genreData.library?.artists ?? [] as artist (artist.local_id || artist.musicbrainz_id || artist.name)}
								<GenreArtistCard
									{artist}
									showLibraryBadge={true}
									href={artistHrefOrNull(artist.musicbrainz_id) ??
										(artist.local_id ? localArtistHref(artist.local_id) : null)}
								/>
							{/each}
						</div>
					{/if}

					{#if (genreData.library?.albums?.length ?? 0) > 0}
						<h3 class="text-lg font-semibold mb-4 text-base-content/70">Albums</h3>
						<div
							class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
						>
							{#each genreData.library?.albums ?? [] as album (album.local_id || album.musicbrainz_id || album.title)}
								<GenreAlbumCard
									{album}
									showLibraryBadge={true}
									href={albumHrefOrNull(album.musicbrainz_id) ??
										(album.local_id ? localAlbumHref(album.local_id) : null)}
								/>
							{/each}
						</div>
					{/if}
				</section>
				<div class="divider my-8 opacity-30"></div>
			{/if}

			<section class="mb-12" aria-label="Popular Artists">
				<div class="flex items-center gap-3 mb-6">
					<div
						class="w-10 h-10 rounded-xl bg-primary/20 flex items-center justify-center text-primary"
					>
						<Mic class="w-5 h-5" />
					</div>
					<div>
						<h2 class="text-2xl font-bold">Popular Artists</h2>
						<p class="text-sm text-base-content/50">Top {genreName} artists</p>
					</div>
				</div>

				{#if popularArtists.length === 0}
					<div class="flex flex-col items-center justify-center py-16">
						<Mic class="h-10 w-10 text-base-content/30 mb-4" strokeWidth={1.5} />
						<p class="text-base-content/50">No artists found for this genre</p>
					</div>
				{:else}
					<div
						class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
					>
						{#each popularArtists as artist (artist.musicbrainz_id || artist.name)}
							<GenreArtistCard {artist} href={artistHrefOrNull(artist.musicbrainz_id)} />
						{/each}
					</div>
					{#if genreQuery.hasNextPage}
						<div class="flex justify-center mt-8">
							<Button
								variant="outline"
								icon={ChevronDown}
								loading={loadingMoreArtists}
								class="w-64"
								onclick={() => genreQuery.fetchNextPage()}
							>
								View More Artists
							</Button>
						</div>
					{/if}
				{/if}
			</section>

			<section class="mb-12" aria-label="Popular Albums">
				<div class="flex items-center gap-3 mb-6">
					<div
						class="w-10 h-10 rounded-xl bg-secondary/20 flex items-center justify-center text-secondary"
					>
						<Disc3 class="w-5 h-5" />
					</div>
					<div>
						<h2 class="text-2xl font-bold">Popular Albums</h2>
						<p class="text-sm text-base-content/50">Top {genreName} albums</p>
					</div>
				</div>

				{#if popularAlbums.length === 0}
					<div class="flex flex-col items-center justify-center py-16">
						<Disc3 class="h-10 w-10 text-base-content/30 mb-4" strokeWidth={1.5} />
						<p class="text-base-content/50">No albums found for this genre</p>
					</div>
				{:else}
					<div
						class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6 gap-4"
					>
						{#each popularAlbums as album (album.musicbrainz_id || album.title)}
							<GenreAlbumCard {album} href={albumHrefOrNull(album.musicbrainz_id)} />
						{/each}
					</div>
					{#if hasMoreAlbums}
						<div class="flex justify-center mt-8">
							<Button
								variant="outline"
								icon={ChevronDown}
								loading={loadingMoreAlbums}
								class="w-64"
								onclick={loadMoreAlbums}
							>
								View More Albums
							</Button>
						</div>
					{/if}
				{/if}
			</section>
		{/if}
	</div>
</div>
