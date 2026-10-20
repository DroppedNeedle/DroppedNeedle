<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import { Disc3, MicVocal } from 'lucide-svelte';
	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import LocalAlbumTrackList from '$lib/components/library/LocalAlbumTrackList.svelte';
	import ArtistAppearancesSectionSkeleton from './ArtistAppearancesSectionSkeleton.svelte';
	import { getLibraryArtistAppearancesQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import { ApiError } from '$lib/api/client';
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
	<section
		id="section-library-appearances"
		class="scroll-mt-24 rounded-2xl border border-error/20 bg-error/5 p-5 {className}"
		aria-labelledby="library-appearances-title"
	>
		<div class="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
			<div>
				<h2 id="library-appearances-title" class="font-bold">Appears in your library</h2>
				<p class="mt-1 text-sm text-base-content/60">
					Couldn't load this artist's local track appearances.
				</p>
			</div>
			<Button variant="outline" size="sm" onclick={() => appearancesQuery.refetch()}>Retry</Button>
		</div>
	</section>
{:else if appearances.length > 0}
	<section
		id="section-library-appearances"
		class="appearance-ledger scroll-mt-24 overflow-hidden rounded-2xl border border-base-content/10 bg-base-200/30 {className}"
		aria-labelledby="library-appearances-title"
	>
		<header class="border-b border-base-content/10 px-4 py-5 sm:px-6">
			<div class="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
				<div class="flex items-start gap-3">
					<div
						class="appearance-mark grid h-11 w-11 shrink-0 place-items-center rounded-xl border border-accent/20 bg-accent/10 text-accent"
						aria-hidden="true"
					>
						<MicVocal class="h-5 w-5" />
					</div>
					<div>
						<p class="text-[10px] font-bold uppercase tracking-[0.18em] text-base-content/45">
							Local credits
						</p>
						<h2 id="library-appearances-title" class="mt-0.5 text-xl font-black tracking-tight">
							Appears in your library
						</h2>
						<p class="mt-1 max-w-2xl text-sm text-base-content/60">
							Exact local tracks credited to this artist, grouped by the release you own.
						</p>
					</div>
				</div>
				<div class="flex shrink-0 items-center gap-2 text-xs">
					<span class="badge badge-outline gap-1.5">
						<Disc3 class="h-3.5 w-3.5" />
						{releaseCount}
						{releaseCount === 1 ? 'release' : 'releases'}
					</span>
					<span class="badge badge-outline badge-accent">
						{trackCount}
						{trackCount === 1 ? 'track' : 'tracks'}
					</span>
				</div>
			</div>
		</header>

		<div class="space-y-3 p-3 sm:p-4">
			{#each appearances as appearance (appearance.album.id)}
				<Card as="article" variant="bordered" padding="sm">
					<div class="grid gap-4 sm:grid-cols-[7rem_minmax(0,1fr)]">
						<a
							href={albumHref(appearance.album.musicbrainz_release_group_id ?? appearance.album.id)}
							class="appearance-cover group/cover relative mx-auto block aspect-square w-28 overflow-hidden rounded-lg shadow-md sm:mx-0 sm:w-full"
							aria-label={`Open ${appearance.album.title}`}
						>
							<AlbumImage
								albumId={appearance.album.id}
								mbid={appearance.album.musicbrainz_release_group_id ?? ''}
								available={appearance.album.cover_available}
								alt={`Cover for ${appearance.album.title}`}
								size="full"
								requestSize={250}
								rounded="none"
								className="h-full w-full transition-transform duration-300 group-hover/cover:scale-[1.03]"
							/>
						</a>

						<div class="min-w-0">
							<div class="min-w-0">
								<p class="text-[10px] font-bold uppercase tracking-[0.16em] text-accent/75">
									Appears on
								</p>
								<a
									href={albumHref(
										appearance.album.musicbrainz_release_group_id ?? appearance.album.id
									)}
									class="mt-0.5 block truncate text-lg font-bold hover:underline"
									>{appearance.album.title}</a
								>
								<p class="mt-0.5 truncate text-xs text-base-content/50">
									{appearance.album.artist_name || 'Unknown album artist'}
									{#if appearance.album.year}
										<span aria-hidden="true"> · </span>{appearance.album.year}
									{/if}
								</p>
							</div>
							<LocalAlbumTrackList tracks={appearance.tracks} />
						</div>
					</div>
				</Card>
			{/each}
		</div>

		{#if appearancesQuery.hasNextPage}
			<footer
				class="flex flex-col items-center justify-between gap-3 border-t border-base-content/10 bg-base-100/35 px-4 py-3 sm:flex-row"
			>
				<p class="text-xs text-base-content/50">
					Showing {appearances.length} of {releaseCount} local releases
				</p>
				<Button
					variant="ghost"
					size="sm"
					loading={appearancesQuery.isFetchingNextPage}
					onclick={() => void appearancesQuery.fetchNextPage()}
					>{appearancesQuery.isFetchingNextPage
						? 'Loading appearances'
						: 'Load more appearances'}</Button
				>
			</footer>
		{/if}
	</section>
{/if}

<style>
	.appearance-ledger {
		background-image: linear-gradient(
			145deg,
			color-mix(in oklab, var(--color-accent) 4%, transparent),
			transparent 38%
		);
	}

	.appearance-mark {
		box-shadow: inset 0 0 0 1px color-mix(in oklab, var(--color-accent) 8%, transparent);
	}

	@media (prefers-reduced-motion: reduce) {
		.appearance-cover :global(img) {
			transition: none !important;
			transform: none !important;
		}
	}
</style>
