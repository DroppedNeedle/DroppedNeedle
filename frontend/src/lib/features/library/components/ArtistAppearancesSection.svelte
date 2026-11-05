<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import { MicVocal } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import LocalAlbumTrackList from '$lib/features/library/components/LocalAlbumTrackList.svelte';
	import ArtistAppearancesSectionSkeleton from './ArtistAppearancesSectionSkeleton.svelte';
	import { getLibraryArtistAppearancesQuery } from '$lib/features/library/queries/LibraryQueries.svelte';
	import { ApiError } from '$lib/api/errors';
	import { albumHref } from '$lib/utils/entityRoutes';

	interface Props {
		artistId: string;
		className?: string;
	}

	let { artistId, className = '' }: Props = $props();
	const appearancesQuery = getLibraryArtistAppearancesQuery(() => artistId);
	const appearances = $derived(appearancesQuery.data?.pages.flatMap((page) => page.items) ?? []);
	// No library artist behind the id: nothing appears, so there is no section.
	const notInLibrary = $derived(
		appearancesQuery.error instanceof ApiError && appearancesQuery.error.status === 404
	);
	const summary = $derived(appearancesQuery.data?.pages[0]);
	const releaseCount = $derived(summary?.total ?? appearances.length);
	const trackCount = $derived(
		summary?.total_tracks ??
			appearances.reduce((total, appearance) => total + appearance.tracks.length, 0)
	);
</script>

{#if appearancesQuery.isLoading}
	<div class={className}>
		<ArtistAppearancesSectionSkeleton />
	</div>
{:else if appearancesQuery.isError && !notInLibrary}
	<Card
		as="section"
		variant="bordered"
		padding="md"
		id="section-library-appearances"
		class="scroll-mt-32 {className}"
		aria-labelledby="library-appearances-title"
	>
		<div class="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
			<div>
				<h2 id="library-appearances-title" class="dn-appear__title">Appears on</h2>
				<p class="mt-1 text-sm text-error">Couldn't load this artist's local track appearances.</p>
			</div>
			<Button variant="outline" size="sm" onclick={() => appearancesQuery.refetch()}>Retry</Button>
		</div>
	</Card>
{:else if appearances.length > 0}
	<Card
		as="section"
		variant="bordered"
		padding="none"
		id="section-library-appearances"
		class="scroll-mt-32 {className}"
		aria-labelledby="library-appearances-title"
	>
		<header class="dn-appear__head">
			<div class="min-w-0">
				<p class="dn-appear__kicker">
					<MicVocal class="size-3.5" aria-hidden="true" />
					Local credits
				</p>
				<h2 id="library-appearances-title" class="dn-appear__title">Appears on</h2>
				<p class="mt-2 max-w-2xl text-sm text-muted">
					Exact local tracks credited to this artist, grouped by the release you own.
				</p>
			</div>
			<p class="dn-appear__count">
				<span>{releaseCount} {releaseCount === 1 ? 'release' : 'releases'}</span>
				<span aria-hidden="true">·</span>
				<span>{trackCount} {trackCount === 1 ? 'track' : 'tracks'}</span>
			</p>
		</header>

		<div class="dn-appear__list">
			{#each appearances as appearance (appearance.album.id)}
				<article class="dn-appear__album">
					<a
						href={albumHref(appearance.album.release_group_mbid ?? appearance.album.id)}
						class="dn-appear__cover"
						aria-label={`Open ${appearance.album.title}`}
					>
						<AlbumImage
							albumId={appearance.album.id}
							mbid={appearance.album.release_group_mbid ?? ''}
							available={appearance.album.cover_available}
							alt={`Cover for ${appearance.album.title}`}
							size="full"
							requestSize={250}
							rounded="none"
							className="h-full w-full"
						/>
					</a>

					<div class="min-w-0">
						<a
							href={albumHref(appearance.album.release_group_mbid ?? appearance.album.id)}
							class="dn-appear__album-title">{appearance.album.title}</a
						>
						<p class="mt-0.5 truncate font-mono text-xs text-muted">
							{appearance.album.artist_name || 'Unknown album artist'}
							{#if appearance.album.year}
								<span aria-hidden="true"> · </span>{appearance.album.year}
							{/if}
						</p>
						<LocalAlbumTrackList tracks={appearance.tracks} />
					</div>
				</article>
			{/each}
		</div>

		{#if appearancesQuery.hasNextPage}
			<footer class="dn-appear__foot">
				<p class="font-mono text-xs tracking-wide text-muted uppercase">
					Showing {appearances.length} of {releaseCount} local releases
				</p>
				<Button
					variant="outline"
					size="sm"
					loading={appearancesQuery.isFetchingNextPage}
					onclick={() => void appearancesQuery.fetchNextPage()}
					>{appearancesQuery.isFetchingNextPage
						? 'Loading appearances'
						: 'Load more appearances'}</Button
				>
			</footer>
		{/if}
	</Card>
{/if}

<style>
	.dn-appear__head {
		display: flex;
		flex-wrap: wrap;
		align-items: flex-end;
		justify-content: space-between;
		gap: 0.75rem 1.5rem;
		padding: 1.25rem 1.25rem 1rem;
		border-bottom: 1px solid var(--dn-line);
	}

	.dn-appear__kicker {
		display: flex;
		align-items: center;
		gap: 0.375rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}

	.dn-appear__title {
		margin-top: 0.25rem;
		font-family: var(--font-display);
		font-size: 2rem;
		line-height: 1;
		color: var(--color-base-content);
	}

	.dn-appear__count {
		display: flex;
		gap: 0.5rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		letter-spacing: 0.14em;
		text-transform: uppercase;
	}

	.dn-appear__list {
		display: grid;
		gap: 0;
	}

	.dn-appear__album {
		display: grid;
		grid-template-columns: 4.5rem minmax(0, 1fr);
		gap: 1rem;
		padding: 1rem 1.25rem;
	}
	.dn-appear__album + .dn-appear__album {
		border-top: 1px solid var(--dn-line);
	}

	.dn-appear__cover {
		display: block;
		aspect-ratio: 1;
		overflow: hidden;
		border-radius: 0.375rem;
	}

	.dn-appear__album-title {
		display: block;
		overflow: hidden;
		font-size: 1.0625rem;
		font-weight: 700;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.dn-appear__album-title:hover {
		text-decoration: underline;
	}

	.dn-appear__foot {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.875rem 1.25rem;
		border-top: 1px solid var(--dn-line);
	}

	@media (min-width: 640px) {
		.dn-appear__head,
		.dn-appear__album,
		.dn-appear__foot {
			padding-inline: 1.5rem;
		}
		.dn-appear__album {
			grid-template-columns: 7rem minmax(0, 1fr);
			gap: 1.25rem;
		}
	}
</style>
