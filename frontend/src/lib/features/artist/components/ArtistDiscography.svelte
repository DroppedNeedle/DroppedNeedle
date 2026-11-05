<!--
	The artist's discography: one chip per kind (Albums, EPs, Singles, with
	their counts) switching a grid of release tiles. Each tile opens the
	release; one not in the library can be sampled and requested, one the
	library has or has asked for carries its badge (an admin's opens the
	remove dialog). "Download all" offers the chosen kind's missing releases,
	and "Load more releases" fetches the next page under the grid.
-->
<script lang="ts" module>
	export type DiscographyKind = 'albums' | 'eps' | 'singles';
</script>

<script lang="ts">
	import { Download } from 'lucide-svelte';
	import { libraryStore } from '$lib/stores/library';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import MediaTile from '$lib/media/MediaTile.svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import SampleButton from '$lib/features/discover/components/SampleButton.svelte';
	import LibraryBadge from '$lib/features/album/components/LibraryBadge.svelte';
	import { getLibraryMembershipQuery } from '$lib/features/library/queries/LibraryQueries.svelte';
	import type { ReleaseGroup } from '$lib/features/album/queries/types';
	import { albumHref } from '$lib/utils/entityRoutes';
	import { trackCountLabel } from '$lib/utils/trackCount';

	interface Props {
		albums: ReleaseGroup[];
		eps: ReleaseGroup[];
		singles: ReleaseGroup[];
		artistName: string;
		requestingIds: Set<string>;
		loadingMore?: boolean;
		hasMore?: boolean;
		loadedCount: number;
		totalCount?: number | null;
		onrequest: (id: string, title?: string) => void;
		onremoved: () => void;
		ondownloadall: (releases: ReleaseGroup[], type: string) => void;
		onloadmore: () => void;
	}

	let {
		albums,
		eps,
		singles,
		artistName,
		requestingIds,
		loadingMore = false,
		hasMore = false,
		loadedCount,
		totalCount = null,
		onrequest,
		onremoved,
		ondownloadall,
		onloadmore
	}: Props = $props();

	const kinds = $derived(
		[
			{ id: 'albums' as const, label: 'Albums', one: 'album', type: 'Album', releases: albums },
			{ id: 'eps' as const, label: 'EPs', one: 'EP', type: 'EP', releases: eps },
			{ id: 'singles' as const, label: 'Singles', one: 'single', type: 'Single', releases: singles }
		].filter((kind) => kind.releases.length > 0)
	);

	let chosen = $state<DiscographyKind | null>(null);
	const current = $derived(kinds.find((kind) => kind.id === chosen) ?? kinds[0] ?? null);

	const membershipQuery = getLibraryMembershipQuery(() =>
		(current?.releases ?? []).map((release) => release.id)
	);
	const membership = $derived(membershipQuery.data);
	const ownedIds = $derived(new Set(membership?.owned_ids ?? []));
	const requestedIds = $derived(new Set(membership?.requested_ids ?? []));
	// What the library holds of each owned group, by lowercase id.
	const heldTracks = $derived(
		new Map((membership?.held_tracks ?? []).map((group) => [group.id, group]))
	);
	// Taken back by an admin's remove, until the page's data catches up.
	let removed = $state<string[]>([]);

	const isOwned = (release: ReleaseGroup) =>
		!removed.includes(release.id) &&
		(ownedIds.has(release.id.toLowerCase()) ||
			libraryStore.isInLibrary(release.id) ||
			(!membership && release.in_library));
	const isRequested = (release: ReleaseGroup) =>
		!removed.includes(release.id) &&
		!isOwned(release) &&
		(requestedIds.has(release.id.toLowerCase()) ||
			release.requested ||
			libraryStore.isRequested(release.id));

	const missing = $derived(
		(current?.releases ?? []).filter((release) => !isOwned(release) && !isRequested(release)).length
	);

	function request(release: ReleaseGroup) {
		removed = removed.filter((id) => id !== release.id);
		onrequest(release.id, release.title);
	}

	function handleDeleted(release: ReleaseGroup) {
		removed = [...removed, release.id];
		onremoved();
	}
</script>

{#if current}
	<section id="section-discography" class="scroll-mt-32" aria-labelledby="artist-discography-title">
		<div class="dn-disco__head">
			<h2 id="artist-discography-title" class="dn-disco__title">Discography</h2>
			<div class="dn-disco__chips" role="group" aria-label="Release kind">
				{#each kinds as kind (kind.id)}
					<Button
						size="sm"
						pill
						variant={kind.id === current.id ? 'primary' : 'outline'}
						aria-pressed={kind.id === current.id}
						onclick={() => (chosen = kind.id)}
					>
						{kind.label} ({kind.releases.length})
					</Button>
				{/each}
			</div>
			{#if missing > 0}
				<Button
					variant="ghost"
					size="sm"
					icon={Download}
					class="dn-disco__all"
					title="Download all {current.label.toLowerCase()}"
					aria-label="Download all {current.label.toLowerCase()}"
					onclick={() => ondownloadall(current.releases, current.type)}
				>
					Download all
				</Button>
			{/if}
		</div>

		<ul id="section-{current.id}" class="dn-disco__grid" aria-label={current.label}>
			{#each current.releases as release (release.id)}
				{@const held = heldTracks.get(release.id.toLowerCase())}
				{@const owned = isOwned(release)}
				{@const requested = isRequested(release)}
				<li class="dn-disco__item">
					<MediaTile title={release.title} href={albumHref(release.id)} titleAs="h3">
						{#snippet art()}
							<AlbumImage
								mbid={release.id}
								alt="{release.title} cover"
								size="full"
								requestSize={250}
								className="h-full w-full"
							/>
						{/snippet}
						{#snippet meta()}
							{#if release.year}{release.year}{/if}{#if held}{#if release.year}<span
										class="mx-1"
										aria-hidden="true">·</span
									>{/if}{trackCountLabel(held.held, held.expected)}{/if}
						{/snippet}
						{#snippet overlay()}
							{#if owned || requested}
								<LibraryBadge
									status={owned ? 'library' : 'requested'}
									musicbrainzId={release.id}
									albumTitle={release.title}
									{artistName}
									size="md"
									positioning="absolute top-2 right-2"
									ondeleted={() => handleDeleted(release)}
								/>
							{/if}
							{#if !owned}
								<div class="dn-disco__tools">
									<SampleButton
										sampleKey={release.id}
										artist={artistName}
										title={release.title}
										kind="album"
										size="sm"
									/>
									{#if !requested}
										<IconButton
											variant="primary"
											size="sm"
											icon={Download}
											label="Request {current.one}"
											loading={requestingIds.has(release.id)}
											onclick={() => request(release)}
										/>
									{/if}
								</div>
							{/if}
						{/snippet}
					</MediaTile>
				</li>
			{/each}
		</ul>

		{#if hasMore || loadingMore}
			<div class="dn-disco__more">
				<p id="artist-release-progress" class="dn-disco__progress" aria-live="polite">
					{#if totalCount}
						{loadedCount} of {totalCount} releases loaded
					{:else}
						{loadedCount} releases loaded
					{/if}
				</p>
				<Button
					variant="outline"
					aria-label="Load more releases"
					aria-describedby="artist-release-progress"
					loading={loadingMore}
					onclick={onloadmore}
				>
					{loadingMore ? 'Loading releases…' : 'Load more releases'}
				</Button>
			</div>
		{/if}
	</section>
{/if}

<style>
	.dn-disco__head {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.75rem 1.5rem;
		margin-bottom: 1.25rem;
	}

	.dn-disco__title {
		font-family: var(--font-display);
		font-size: clamp(2.25rem, 1.6rem + 1.8vw, 3rem);
		line-height: 1;
		color: var(--color-base-content);
	}

	.dn-disco__chips {
		display: flex;
		gap: 0.5rem;
	}

	.dn-disco__head :global(.dn-disco__all) {
		margin-left: auto;
	}

	.dn-disco__grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(min(9.5rem, calc(50% - 0.5rem)), 1fr));
		gap: 1.5rem 1.125rem;
	}

	.dn-disco__tools {
		position: absolute;
		right: 0.5rem;
		bottom: 0.5rem;
		display: flex;
		gap: 0.375rem;
		transition: opacity 150ms;
	}
	@media (hover: hover) and (pointer: fine) {
		.dn-disco__tools {
			opacity: 0;
		}
		.dn-disco__item:hover .dn-disco__tools,
		.dn-disco__item:focus-within .dn-disco__tools {
			opacity: 1;
		}
	}

	.dn-disco__more {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.75rem;
		margin-top: 2rem;
	}

	.dn-disco__progress {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}

	@media (max-width: 639px) {
		.dn-disco__head {
			gap: 0.75rem;
		}
		.dn-disco__chips {
			order: 3;
			width: 100%;
			overflow-x: auto;
			scrollbar-width: none;
		}
		.dn-disco__chips > :global(*) {
			flex: none;
		}
		.dn-disco__grid {
			gap: 1.25rem 0.875rem;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.dn-disco__tools {
			transition: none;
		}
	}
</style>
