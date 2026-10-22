<script lang="ts">
	import { goto } from '$app/navigation';
	import { ChevronLeft, Disc3, ExternalLink, FileSearch, FileUp, MicVocal } from 'lucide-svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import LibraryAlbumCard from '$lib/components/library/LibraryAlbumCard.svelte';
	import ArtistAppearancesSection from '$lib/components/library/ArtistAppearancesSection.svelte';
	import ArtistMergeDialog from '$lib/components/library/ArtistMergeDialog.svelte';
	import LocalIdentityBadge from '$lib/components/library/LocalIdentityBadge.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import type { ContributionState, LibraryAlbumSummary } from '$lib/types';
	import {
		getLibraryArtistAlbumsQuery,
		getLibraryArtistDetailQuery
	} from '$lib/queries/library/LibraryQueries.svelte';
	import { createLibraryContributionMutation } from '$lib/queries/libraryContributions/LibraryContributionMutations.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import RatingStars from '$lib/ui/RatingStars.svelte';
	import { getRatingsQuery, ratingOf, setRating } from '$lib/queries/ratings/RatingQueries.svelte';

	interface Props {
		artistId: string;
	}

	let { artistId }: Props = $props();
	const artistQuery = getLibraryArtistDetailQuery(() => artistId);
	const albumsQuery = getLibraryArtistAlbumsQuery(() => artistId);
	const artist = $derived(artistQuery.data);
	const albums = $derived(albumsQuery.data?.items ?? []);
	const contributionAlbums = $derived(
		albums.filter((album) => album.album_identity_state === 'local_only')
	);
	const contributionMutation = createLibraryContributionMutation();
	const ratingsQuery = getRatingsQuery(() => (artist ? { artistIds: [artist.id] } : null));
	const rate = setRating();

	function openContribution(album: LibraryAlbumSummary): void {
		if (album.contribution_id) {
			void goto(withBasePath(`/library/contributions/${album.contribution_id}`));
			return;
		}
		contributionMutation.mutate(album.id);
	}

	function contributionLabel(state: ContributionState | null): string {
		if (state === 'draft' || state === 'ready') return 'Draft in progress';
		if (state === 'seeded') return 'Waiting for MusicBrainz';
		if (state === 'verifying') return 'Verifying MusicBrainz';
		if (state === 'needs_review') return 'Needs review';
		return 'Not started';
	}
</script>

<svelte:head><title>{artist?.name ?? 'Artist'} · Library</title></svelte:head>

<main class="container mx-auto p-4 md:p-6 lg:p-8">
	<Button
		variant="ghost"
		size="sm"
		icon={ChevronLeft}
		class="mb-5"
		onclick={() => goto(withBasePath('/library/artists'))}>Artists</Button
	>
	{#if artistQuery.isLoading}
		<div class="flex items-center gap-5">
			<div class="skeleton h-32 w-32 rounded-full"></div>
			<div class="skeleton h-12 w-64"></div>
		</div>
	{:else if artistQuery.isError || !artist}
		<div class="alert alert-error">
			<span>Couldn't load this artist.</span>
			<Button size="sm" onclick={() => artistQuery.refetch()}>Retry</Button>
		</div>
	{:else}
		<header class="flex flex-col gap-5 sm:flex-row sm:items-end">
			<ArtistImage
				mbid={artist.musicbrainz_artist_id ?? ''}
				localId={artist.id}
				alt={artist.name}
				size="xl"
				requestSize={250}
				priority
				className="shadow-xl"
			/>
			<div class="min-w-0 flex-1">
				<h1 class="text-4xl font-black tracking-tight sm:text-5xl">{artist.name}</h1>
				<RatingStars
					size="md"
					class="mt-2 -ml-1"
					value={ratingOf(ratingsQuery.data, 'artist', artist.id)}
					label={artist.name}
					onchange={(rating) => rate.mutate({ kind: 'artist', itemId: artist.id, rating })}
				/>
				<LocalIdentityBadge
					state={artist.artist_identity_state}
					subject="artist"
					showDescription
					className="mt-3"
				/>
				<div class="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-base-content/55">
					{#if artist.album_count > 0}
						<span>
							{artist.album_count}
							{artist.album_count === 1 ? 'album' : 'albums'} · {artist.track_count}
							{artist.track_count === 1 ? 'track' : 'tracks'}
						</span>
					{/if}
					{#if artist.appearance_track_count > 0}
						<span class="inline-flex items-center gap-1.5 text-accent">
							<MicVocal class="h-3.5 w-3.5" />
							Appears on {artist.appearance_track_count}
							{artist.appearance_track_count === 1 ? 'local track' : 'local tracks'}
						</span>
					{/if}
				</div>
				{#if artist.musicbrainz_artist_id}
					<Button
						variant="ghost"
						size="sm"
						icon={ExternalLink}
						iconPosition="end"
						class="mt-3"
						href={`https://musicbrainz.org/artist/${artist.musicbrainz_artist_id}`}
						target="_blank"
						rel="noreferrer">Open in MusicBrainz</Button
					>
				{/if}
				{#if authStore.isTrusted && artist.artist_identity_state === 'local_only' && contributionAlbums.length}
					<Button
						variant="outline"
						size="sm"
						icon={FileSearch}
						class="mt-3"
						href="#musicbrainz-albums"
					>
						Find existing MusicBrainz artist
					</Button>
				{/if}
			</div>
			{#if authStore.isAdmin}<ArtistMergeDialog {artist} />{/if}
		</header>

		{#if authStore.isTrusted && contributionAlbums.length}
			<Card
				as="section"
				variant="bordered"
				padding="none"
				id="musicbrainz-albums"
				class="mt-8 scroll-mt-6 overflow-hidden"
				aria-label="Contribute through an album"
			>
				<div class="border-b border-base-content/10 p-4 sm:p-5">
					<div class="flex items-start gap-3">
						<div class="rounded-box bg-primary/10 p-2 text-primary" aria-hidden="true">
							<FileUp class="h-5 w-5" />
						</div>
						<div>
							<h2 class="font-bold">Contribute through an album</h2>
							<p class="mt-1 max-w-3xl text-sm text-base-content/65">
								Use one of this artist's local albums to check MusicBrainz. If the release matches,
								DroppedNeedle can also verify and link its artist credit.
							</p>
						</div>
					</div>
				</div>
				<ul class="divide-y divide-base-content/10">
					{#each contributionAlbums as album (album.id)}
						<li
							class="flex flex-col gap-3 p-4 sm:flex-row sm:items-center sm:justify-between sm:px-5"
						>
							<div class="min-w-0">
								<a class="font-semibold hover:underline" href={withBasePath(`/album/${album.id}`)}
									>{album.title}</a
								>
								<p class="mt-0.5 text-xs text-base-content/55">
									{album.year ?? 'Year unknown'} · {contributionLabel(album.contribution_state)}
								</p>
							</div>
							<Button
								variant="ghost"
								size="sm"
								icon={FileSearch}
								class="shrink-0 sm:self-center"
								disabled={contributionMutation.isPending}
								onclick={() => openContribution(album)}
							>
								{album.contribution_id ? 'Resume' : 'Start with this album'}
							</Button>
						</li>
					{/each}
				</ul>
			</Card>
		{/if}

		{#if artist.appearance_track_count > 0}
			<ArtistAppearancesSection artistId={artist.id} className="mt-8" />
		{/if}

		<section class="mt-8" aria-labelledby="artist-albums-title">
			<div class="flex items-center gap-2">
				<Disc3 class="h-5 w-5 text-primary" />
				<h2 id="artist-albums-title" class="text-xl font-bold">Albums</h2>
			</div>
			{#if albumsQuery.isLoading}<div
					class="mt-4 grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5"
				>
					{#each Array(5) as _, index (index)}<div class="skeleton aspect-square"></div>{/each}
				</div>{:else if albumsQuery.isError}<div class="alert alert-error mt-4">
					Couldn't load this artist's albums.
				</div>{:else if !albumsQuery.data?.items.length}<p class="mt-4 text-base-content/55">
					No albums in your library are credited to this artist.
				</p>{:else}<div
					class="mt-4 grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5"
				>
					{#each albumsQuery.data.items as album (album.id)}<LibraryAlbumCard {album} />{/each}
				</div>{/if}
		</section>
	{/if}
</main>
