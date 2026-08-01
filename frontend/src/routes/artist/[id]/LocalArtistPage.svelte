<script lang="ts">
	import { goto } from '$app/navigation';
	import { ChevronLeft, Disc3, ExternalLink, FileSearch, FileUp } from 'lucide-svelte';
	import ArtistImage from '$lib/components/ArtistImage.svelte';
	import LibraryAlbumCard from '$lib/components/library/LibraryAlbumCard.svelte';
	import ArtistMergeDialog from '$lib/components/library/ArtistMergeDialog.svelte';
	import LocalIdentityBadge from '$lib/components/library/LocalIdentityBadge.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import type { ContributionState, LibraryAlbumSummary } from '$lib/types';
	import {
		getLibraryArtistAlbumsQuery,
		getLibraryArtistDetailQuery
	} from '$lib/queries/library/LibraryQueries.svelte';
	import { createLibraryContributionMutation } from '$lib/queries/libraryContributions/LibraryContributionMutations.svelte';

	interface Props {
		artistId: string;
	}

	let { artistId }: Props = $props();
	const artistQuery = getLibraryArtistDetailQuery(() => artistId);
	const albumsQuery = getLibraryArtistAlbumsQuery(() => artistId);
	const artist = $derived(artistQuery.data);
	const albums = $derived(albumsQuery.data?.items ?? []);
	const contributionAlbums = $derived(
		albums.filter((album) => album.album_identity_state !== 'release_linked')
	);
	const contributionMutation = createLibraryContributionMutation();

	function openContribution(album: LibraryAlbumSummary): void {
		if (album.contribution_id) {
			void goto(`/library/contributions/${album.contribution_id}`);
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
	<button class="btn btn-ghost btn-sm mb-5 gap-2" onclick={() => goto('/library/artists')}
		><ChevronLeft class="h-4 w-4" /> Artists</button
	>
	{#if artistQuery.isLoading}
		<div class="flex items-center gap-5">
			<div class="skeleton h-32 w-32 rounded-full"></div>
			<div class="skeleton h-12 w-64"></div>
		</div>
	{:else if artistQuery.isError || !artist}
		<div class="alert alert-error">
			<span>Couldn't load this artist.</span><button
				class="btn btn-sm"
				onclick={() => artistQuery.refetch()}>Retry</button
			>
		</div>
	{:else}
		<header class="flex flex-col gap-5 sm:flex-row sm:items-end">
			<ArtistImage
				mbid={artist.id}
				source="local"
				available={artist.musicbrainz_artist_id !== null}
				alt={artist.name}
				size="xl"
				className="shadow-xl"
			/>
			<div class="min-w-0 flex-1">
				<h1 class="text-4xl font-black tracking-tight sm:text-5xl">{artist.name}</h1>
				<LocalIdentityBadge
					state={artist.artist_identity_state}
					subject="artist"
					showDescription
					className="mt-3"
				/>
				<p class="mt-2 text-sm text-base-content/55">
					{artist.album_count}
					{artist.album_count === 1 ? 'album' : 'albums'} · {artist.track_count}
					{artist.track_count === 1 ? 'track' : 'tracks'}
				</p>
				{#if artist.musicbrainz_artist_id}
					<div class="mt-3 flex flex-wrap gap-2">
						<a
							class="btn btn-outline btn-sm gap-2"
							href={`/artist/${artist.musicbrainz_artist_id}?view=discography`}
							><Disc3 class="h-4 w-4" /> Full discography</a
						>
						<a
							class="btn btn-ghost btn-sm gap-2"
							href={`https://musicbrainz.org/artist/${artist.musicbrainz_artist_id}`}
							target="_blank"
							rel="noreferrer">Open in MusicBrainz <ExternalLink class="h-3.5 w-3.5" /></a
						>
					</div>
				{/if}
				{#if authStore.isTrusted && artist.artist_identity_state === 'local_only' && contributionAlbums.length}
					<a class="btn btn-outline btn-sm mt-3 gap-2" href="#musicbrainz-albums">
						<FileSearch class="h-4 w-4" /> Find existing MusicBrainz artist
					</a>
				{/if}
			</div>
			{#if authStore.isAdmin}<ArtistMergeDialog {artist} />{/if}
		</header>

		{#if authStore.isTrusted && contributionAlbums.length}
			<section
				id="musicbrainz-albums"
				class="mt-8 scroll-mt-6 overflow-hidden rounded-box border border-primary/20 bg-base-200/35"
				aria-labelledby="musicbrainz-albums-title"
			>
				<div class="border-b border-base-content/10 p-4 sm:p-5">
					<div class="flex items-start gap-3">
						<div class="rounded-box bg-primary/10 p-2 text-primary" aria-hidden="true">
							<FileUp class="h-5 w-5" />
						</div>
						<div>
							<h2 id="musicbrainz-albums-title" class="font-bold">Contribute through an album</h2>
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
								<a class="font-semibold hover:underline" href={`/album/${album.id}`}
									>{album.title}</a
								>
								<p class="mt-0.5 text-xs text-base-content/55">
									{album.year ?? 'Year unknown'} · {contributionLabel(album.contribution_state)}
								</p>
							</div>
							<button
								class="btn btn-ghost btn-sm shrink-0 gap-2 sm:self-center"
								disabled={contributionMutation.isPending}
								onclick={() => openContribution(album)}
							>
								<FileSearch class="h-4 w-4" />
								{album.contribution_id ? 'Resume' : 'Start with this album'}
							</button>
						</li>
					{/each}
				</ul>
			</section>
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
