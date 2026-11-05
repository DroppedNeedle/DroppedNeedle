<!--
	"In your library" on an artist page: a paper card with the artist's
	newest owned album large (cover, title, year, kind, tracks, its format
	and Play album, View tracks and a menu), then the rest of what the
	library holds of them as small tiles, each labelled with its kind when it
	is not a plain album.
-->
<script lang="ts">
	import { Play } from 'lucide-svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import Button from '$lib/ui/Button.svelte';
	import ContextMenu, { type MenuItem } from '$lib/ui/ContextMenu.svelte';
	import HorizontalCarousel from '$lib/ui/HorizontalCarousel.svelte';
	import PaperPanel from '$lib/ui/PaperPanel.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import LibraryAlbumCard from './LibraryAlbumCard.svelte';
	import LibraryFormatBadge from './LibraryFormatBadge.svelte';
	import { getLibraryArtistAlbumsQuery } from '$lib/features/library/queries/LibraryQueries.svelte';
	import { getArtistOwnedReleasesQuery } from '$lib/features/artist/queries/ArtistQueries.svelte';
	import type { AlbumView } from '$lib/features/library/queries/types';
	import { albumHref } from '$lib/utils/entityRoutes';
	import { trackCountLabel } from '$lib/utils/trackCount';
	import { fileOwnedAlbums } from './artistLibrarySections';

	interface Props {
		/** The library artist's id. */
		libraryArtistId: string;
		/** The artist's MusicBrainz id, for the release types; none for a local artist. */
		artistMbid?: string | null;
		/** Plays an owned album; none hides Play album and its menu. */
		onplay?: (album: AlbumView, kind: string) => void;
		/** Whether that play is loading. */
		playing?: boolean;
		/** The featured album's menu. */
		menu?: (album: AlbumView, kind: string) => MenuItem[];
		/** Layout only. */
		class?: string;
	}

	let {
		libraryArtistId,
		artistMbid = null,
		onplay,
		playing = false,
		menu,
		class: className = ''
	}: Props = $props();

	const albumsQuery = getLibraryArtistAlbumsQuery(() => libraryArtistId);
	const ownedQuery = getArtistOwnedReleasesQuery(() => artistMbid ?? '');

	// Without the types (MusicBrainz down, or a local artist) the albums still
	// show, filed by their compilation flag.
	const sections = $derived(
		fileOwnedAlbums(albumsQuery.data?.items ?? [], ownedQuery.data?.items ?? [])
	);
	const filed = $derived([...sections.albums, ...sections.epsAndSingles, ...sections.other]);
	const featured = $derived(filed[0] ?? null);
	const rest = $derived(filed.slice(1));
	const loading = $derived(albumsQuery.isLoading || (!!artistMbid && ownedQuery.isLoading));

	const trackTotal = $derived(filed.reduce((sum, item) => sum + item.album.track_count, 0));
	const formats = $derived(
		filed
			.map((item) => item.album.format?.toUpperCase() ?? '')
			.filter((format, index, all) => format && all.indexOf(format) === index)
			.join(' / ')
	);

	const kind = $derived(featured?.label ?? 'Album');
	// Playing needs the album's MusicBrainz group, which the players match on.
	const canPlay = $derived(!!onplay && !!featured?.album.release_group_mbid);
</script>

<section
	id="section-library-albums"
	class="scroll-mt-32 {className}"
	aria-labelledby="library-albums-title"
>
	<PaperPanel>
		<div class="dn-owned">
			<header class="dn-owned__head">
				<h2 id="library-albums-title" class="dn-owned__title">In your library</h2>
				{#if filed.length > 0}
					<p class="dn-owned__count">
						<span>{filed.length} {filed.length === 1 ? 'album' : 'albums'}</span>
						<span aria-hidden="true">·</span>
						<span>{trackTotal} {trackTotal === 1 ? 'track' : 'tracks'}</span>
						{#if formats}<span aria-hidden="true">·</span><span>{formats}</span>{/if}
					</p>
				{/if}
			</header>

			{#if loading}
				<div class="dn-owned__feature" aria-busy="true">
					<div class="skeleton aspect-square w-full"></div>
					<div class="space-y-3">
						<div class="skeleton h-10 w-3/4"></div>
						<div class="skeleton h-4 w-1/2"></div>
						<div class="skeleton h-10 w-48"></div>
					</div>
				</div>
			{:else if albumsQuery.isError}
				<p class="text-sm" role="alert">Couldn't load this artist's albums.</p>
			{:else if !featured}
				<p class="text-sm text-muted">No albums in your library are credited to this artist.</p>
			{:else}
				{@const album = featured.album}
				<div class="dn-owned__feature">
					<a
						href={albumHref(album.release_group_mbid ?? album.id)}
						class="dn-owned__cover"
						aria-label="Open {album.title}"
					>
						<AlbumImage
							albumId={album.id}
							mbid={album.release_group_mbid ?? ''}
							available={album.cover_available}
							artMissing={album.art_missing}
							alt=""
							size="full"
							requestSize={500}
							rounded="none"
							className="aspect-square w-full"
						/>
					</a>
					<div class="min-w-0">
						<h3 class="dn-owned__album">{album.title}</h3>
						<p class="dn-owned__meta">
							{#if album.year}<span>{album.year}</span><span aria-hidden="true">·</span>{/if}
							<span>{kind}</span>
							<span aria-hidden="true">·</span>
							<span>{trackCountLabel(album.track_count, album.expected_track_count)}</span>
						</p>
						<div class="mt-3 flex flex-wrap items-center gap-2">
							<LibraryFormatBadge format={album.format} />
							<StatusChip tone="action">In library</StatusChip>
						</div>
						<div class="mt-4 flex flex-wrap items-center gap-2">
							{#if canPlay}
								<Button
									variant="primary"
									icon={Play}
									class="[&_svg]:fill-current"
									loading={playing}
									onclick={() => onplay?.(album, kind)}>Play album</Button
								>
							{/if}
							<Button variant="outline" href={albumHref(album.release_group_mbid ?? album.id)}
								>View tracks</Button
							>
							{#if canPlay && menu}
								<ContextMenu
									items={menu(album, kind)}
									trigger="button"
									label="More actions for {album.title}"
								/>
							{/if}
						</div>
					</div>
				</div>

				{#if rest.length > 0}
					<div class="dn-owned__rest">
						<HorizontalCarousel>
							{#each rest as item (item.album.id)}
								<div class="w-32 shrink-0 sm:w-36">
									<LibraryAlbumCard album={item.album} kind={item.label ?? undefined} />
								</div>
							{/each}
						</HorizontalCarousel>
					</div>
				{/if}
			{/if}
		</div>
	</PaperPanel>
</section>

<style>
	.dn-owned {
		padding: 1.25rem;
	}
	@media (min-width: 640px) {
		.dn-owned {
			padding: 1.5rem;
		}
	}

	.dn-owned__head {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 0.25rem 1.25rem;
		margin-bottom: 1rem;
	}

	.dn-owned__title {
		font-family: var(--font-display);
		font-size: 2.25rem;
		line-height: 0.95;
		color: var(--dn-title);
	}

	.dn-owned__count {
		display: flex;
		flex-wrap: wrap;
		gap: 0.5rem;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}

	.dn-owned__feature {
		display: grid;
		grid-template-columns: minmax(0, 9rem) minmax(0, 1fr);
		gap: 1.25rem;
		align-items: start;
	}
	@media (min-width: 640px) {
		.dn-owned__feature {
			grid-template-columns: minmax(0, 13rem) minmax(0, 1fr);
			gap: 1.5rem;
		}
	}
	@media (min-width: 1280px) {
		.dn-owned__feature {
			grid-template-columns: minmax(0, 12rem) minmax(0, 1fr);
		}
	}

	.dn-owned__cover {
		display: block;
		overflow: hidden;
		border-radius: 0.375rem;
		box-shadow:
			0 0 0 1px color-mix(in oklab, var(--dn-paper-content) 14%, transparent),
			0 10px 22px -14px rgb(0 0 0 / 0.55);
	}

	.dn-owned__album {
		font-family: var(--font-display);
		font-size: clamp(1.75rem, 1.2rem + 1.6vw, 2.75rem);
		line-height: 0.95;
		text-transform: uppercase;
		overflow-wrap: anywhere;
		font-synthesis: none;
	}

	.dn-owned__meta {
		display: flex;
		flex-wrap: wrap;
		gap: 0.25rem 0.5rem;
		margin-top: 0.625rem;
		font-family: var(--font-mono);
		font-size: 0.875rem;
	}

	.dn-owned__rest {
		margin-top: 1.5rem;
		padding-top: 1.25rem;
		border-top: 1px solid var(--dn-paper-border);
	}

	@media (max-width: 479px) {
		.dn-owned__feature {
			grid-template-columns: minmax(0, 1fr);
		}
		.dn-owned__cover {
			width: min(100%, 14rem);
		}
	}
</style>
