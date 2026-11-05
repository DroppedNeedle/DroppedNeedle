<script lang="ts">
	import { goto } from '$app/navigation';
	import { ArrowLeft, ExternalLink, FileSearch, FileUp } from 'lucide-svelte';
	import ArtistHero from './ArtistHero.svelte';
	import ArtistLibraryAlbums from '$lib/features/library/components/ArtistLibraryAlbums.svelte';
	import ArtistAppearancesSection from '$lib/features/library/components/ArtistAppearancesSection.svelte';
	import ArtistMergeDialog from '$lib/features/library/components/ArtistMergeDialog.svelte';
	import ArtistMusicBrainzLink from '$lib/features/library/components/ArtistMusicBrainzLink.svelte';
	import LocalIdentityBadge from '$lib/features/library/components/LocalIdentityBadge.svelte';
	import FavoriteHeart from '$lib/features/library/components/FavoriteHeart.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import type { AlbumView } from '$lib/features/library/queries/types';
	import {
		getLibraryArtistAlbumsQuery,
		getLibraryArtistDetailQuery
	} from '$lib/features/library/queries/LibraryQueries.svelte';
	import { createLibraryContributionMutation } from '$lib/features/library/queries/LibraryContributionMutations.svelte';
	import {
		getRatingsQuery,
		ratingOf,
		createSetRatingMutation
	} from '$lib/features/library/queries/ratings.svelte';
	import { createSetFavoriteMutation } from '$lib/features/library/queries/FavoriteMutations.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import JumpTabs, { type JumpSection } from '$lib/ui/JumpTabs.svelte';
	import RatingStars from '$lib/ui/RatingStars.svelte';
	import type { Stat } from '$lib/ui/StatRow.svelte';

	interface Props {
		artistId: string;
	}

	let { artistId }: Props = $props();
	const artistQuery = getLibraryArtistDetailQuery(() => artistId);
	const albumsQuery = getLibraryArtistAlbumsQuery(() => artistId);
	const artist = $derived(artistQuery.data);
	const albums = $derived(albumsQuery.data?.items ?? []);
	const contributionAlbums = $derived(
		albums.filter((album) => album.identity_state === 'local_only')
	);
	const contributing = $derived(authStore.isTrusted && contributionAlbums.length > 0);
	const contributionMutation = createLibraryContributionMutation();
	const ratingsQuery = getRatingsQuery(() => (artist ? { artistIds: [artist.id] } : null));
	const rate = createSetRatingMutation();
	const heart = createSetFavoriteMutation();

	function openContribution(album: AlbumView): void {
		if (album.contribution_id) {
			void goto(withBasePath(`/library/contributions/${album.contribution_id}`));
			return;
		}
		contributionMutation.mutate(album.id);
	}

	function contributionLabel(state: AlbumView['contribution_state']): string {
		if (state === 'draft' || state === 'ready') return 'Draft in progress';
		if (state === 'seeded') return 'Waiting for MusicBrainz';
		if (state === 'verifying') return 'Verifying MusicBrainz';
		if (state === 'needs_review') return 'Needs review';
		return 'Not started';
	}

	const facts = $derived.by<string[]>(() => {
		if (!artist) return [];
		const line: string[] = [];
		if (artist.disambiguation) line.push(artist.disambiguation);
		if (artist.album_count > 0)
			line.push(
				`${artist.album_count} ${artist.album_count === 1 ? 'album' : 'albums'} · ${artist.led_track_count} ${artist.led_track_count === 1 ? 'track' : 'tracks'}`
			);
		if (artist.appearance_track_count > 0)
			line.push(
				`Appears on ${artist.appearance_track_count} ${artist.appearance_track_count === 1 ? 'local track' : 'local tracks'}`
			);
		return line;
	});

	const stats = $derived.by<Stat[]>(() =>
		artist
			? [
					{ label: 'Albums', value: artist.album_count },
					{ label: 'Tracks', value: artist.led_track_count },
					{ label: 'Guest tracks', value: artist.appearance_track_count }
				].filter((stat) => stat.value > 0)
			: []
	);

	const sections = $derived.by<JumpSection[]>(() =>
		artist
			? [
					{ id: 'section-overview', label: 'Overview' },
					{ id: 'section-library-albums', label: 'In your library' },
					...(artist.appearance_track_count > 0
						? [{ id: 'section-library-appearances', label: 'Appears on' }]
						: []),
					...(contributing ? [{ id: 'musicbrainz-albums', label: 'Contribute' }] : [])
				]
			: []
	);
</script>

<svelte:head><title>{artist?.name ?? 'Artist'} · Library</title></svelte:head>

{#snippet back()}
	<IconButton
		variant="neutral"
		icon={ArrowLeft}
		label="Artists"
		onclick={() => goto(withBasePath('/library/artists'))}
	/>
{/snippet}

<main class="w-full">
	{#if artistQuery.isLoading}
		<div class="dn-artist-page">
			{@render back()}
			<div class="mt-8 space-y-4">
				<div class="skeleton h-24 w-2/3 max-w-xl"></div>
				<div class="skeleton h-4 w-64"></div>
			</div>
		</div>
	{:else if artistQuery.isError || !artist}
		<div class="dn-artist-page">
			{@render back()}
			<div class="alert alert-error mt-6">
				<span>Couldn't load this artist.</span>
				<Button size="sm" onclick={() => artistQuery.refetch()}>Retry</Button>
			</div>
		</div>
	{:else}
		<ArtistHero
			name={artist.name}
			mbid={artist.artist_mbid}
			localId={artist.id}
			artMissing={artist.art_missing}
			{facts}
			{stats}
			{back}
		>
			{#snippet actions()}
				<FavoriteHeart
					size="md"
					favorite={artist.favorite ?? false}
					onchange={(favorited) => heart.mutate({ kind: 'artist', itemId: artist.id, favorited })}
				/>
				<RatingStars
					size="md"
					value={ratingOf(ratingsQuery.data, 'artist', artist.id)}
					label={artist.name}
					onchange={(rating) => rate.mutate({ kind: 'artist', itemId: artist.id, rating })}
				/>
				{#if artist.artist_mbid}
					<Button
						variant="outline"
						icon={ExternalLink}
						iconPosition="end"
						href={`https://musicbrainz.org/artist/${artist.artist_mbid}`}
						target="_blank"
						rel="noreferrer">Open in MusicBrainz</Button
					>
				{/if}
				{#if contributing && artist.identity_state === 'local_only'}
					<Button variant="outline" icon={FileSearch} href="#musicbrainz-albums">
						Find existing MusicBrainz artist
					</Button>
				{/if}
				{#if authStore.isAdmin}<ArtistMergeDialog {artist} />{/if}
			{/snippet}
			{#snippet below()}
				<LocalIdentityBadge
					state={artist.identity_state}
					subject="artist"
					showDescription
					className="max-w-xl"
				/>
				<ArtistMusicBrainzLink artistId={artist.id} />
			{/snippet}
		</ArtistHero>

		<JumpTabs {sections} label="Artist page sections" />

		<div class="dn-artist-page space-y-10">
			<ArtistLibraryAlbums libraryArtistId={artist.id} />

			{#if artist.appearance_track_count > 0}
				<ArtistAppearancesSection artistId={artist.id} />
			{/if}

			{#if contributing}
				<Card
					as="section"
					variant="bordered"
					padding="none"
					id="musicbrainz-albums"
					class="scroll-mt-32 overflow-hidden"
					aria-label="Contribute through an album"
				>
					<div class="border-b border-line p-5 sm:p-6">
						<div class="flex items-start gap-3">
							<FileUp class="mt-1 size-5 shrink-0 text-title" aria-hidden="true" />
							<div>
								<h2 class="dn-artist-card-title">Contribute through an album</h2>
								<p class="mt-2 max-w-3xl text-sm text-muted">
									Use one of this artist's local albums to check MusicBrainz. If the release
									matches, DroppedNeedle can also verify and link its artist credit.
								</p>
							</div>
						</div>
					</div>
					<ul class="divide-y divide-line">
						{#each contributionAlbums as album (album.id)}
							<li
								class="flex flex-col gap-3 p-4 sm:flex-row sm:items-center sm:justify-between sm:px-6"
							>
								<div class="min-w-0">
									<a class="font-semibold hover:underline" href={withBasePath(`/album/${album.id}`)}
										>{album.title}</a
									>
									<p class="mt-0.5 font-mono text-xs text-muted">
										{album.year ?? 'Year unknown'} · {contributionLabel(album.contribution_state)}
									</p>
								</div>
								<Button
									variant="outline"
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
		</div>
	{/if}
</main>

<style>
	.dn-artist-page {
		width: 100%;
		max-width: 80rem;
		margin: 0 auto;
		padding: 1.5rem 1rem 2rem;
	}
	@media (min-width: 768px) {
		.dn-artist-page {
			padding-inline: 1.5rem;
		}
	}
	@media (min-width: 1024px) {
		.dn-artist-page {
			padding-inline: 2rem;
		}
	}

	.dn-artist-card-title {
		font-family: var(--font-display);
		font-size: 1.75rem;
		line-height: 1;
		color: var(--color-base-content);
	}
</style>
