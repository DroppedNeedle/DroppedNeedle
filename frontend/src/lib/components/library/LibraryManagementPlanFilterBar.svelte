<!--
	The filters over a preview's file list: outcome, reason, root, format and
	change, then artist or release, collision class and preservation behind a
	disclosure. Counts come from the preview's summary.
-->
<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { getLibrarySearchQuery } from '$lib/queries/library/LibraryQueries.svelte';
	import type { LibraryManagementPreviewDetailResponse } from '$lib/queries/library-management/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Input from '$lib/ui/Input.svelte';
	import {
		managementRootLabel,
		qualifiedManagementReasonLabel,
		titleManagementValue
	} from './LibraryManagementDisplay';
	import type { PreviewFilters } from './previewFilters.svelte';

	interface Props {
		filters: PreviewFilters;
		preview: LibraryManagementPreviewDetailResponse;
		roots: ReadonlyArray<{ id: string; label: string }>;
	}

	let { filters, preview, roots }: Props = $props();

	const collisionClasses = [
		'same_path_same_content',
		'same_path_different_content',
		'same_release_position_different_content',
		'normalized_path_collision',
		'normalized_catalog_path_collision',
		'sidecar_path_collision',
		'destination_created_after_preview'
	];

	const catalogSearchQuery = getLibrarySearchQuery(() => filters.catalogSearch);
	const summary = $derived(preview.summary);
	const byCount = (left: [string, number], right: [string, number]) => right[1] - left[1];
	const sortedReasons = $derived(Object.entries(summary.reasons ?? {}).sort(byCount));
	const sortedRoots = $derived(Object.entries(summary.roots ?? {}).sort(byCount));
	const sortedFormats = $derived(Object.entries(summary.formats ?? {}).sort(byCount));
</script>

<Card as="section" variant="bordered" padding="md" aria-labelledby="preview-filters-title">
	<div class="flex flex-wrap items-end justify-between gap-2">
		<div>
			<p class="management-step">Inspectable plan</p>
			<h2 id="preview-filters-title" class="font-display text-xl font-semibold">
				Files and changes
			</h2>
		</div>
		<span class="text-xs text-base-content/45">Root labels and relative paths only</span>
	</div>
	<div class="mt-2 grid gap-2 sm:grid-cols-2 lg:grid-cols-5">
		<label class="grid gap-1 text-xs"
			><span>Outcome</span><select class="select select-sm" bind:value={filters.eligibility}
				><option value="">All outcomes</option><option value="eligible"
					>Eligible ({(summary.eligible_count ?? 0).toLocaleString()})</option
				><option value="warning">Warning ({(summary.warning_count ?? 0).toLocaleString()})</option
				><option value="blocked">Blocked ({(summary.blocked_count ?? 0).toLocaleString()})</option
				><option value="stale">Stale ({(summary.stale_count ?? 0).toLocaleString()})</option
				></select
			></label
		>
		<label class="grid gap-1 text-xs"
			><span>Reason</span><select class="select select-sm" bind:value={filters.reasonCode}
				><option value="">All reasons</option
				>{#each sortedReasons as [reason, count] (reason)}<option value={reason}
						>{qualifiedManagementReasonLabel(reason)} ({count.toLocaleString()})</option
					>{/each}</select
			></label
		>
		<label class="grid gap-1 text-xs"
			><span>Root</span><select class="select select-sm" bind:value={filters.rootId}
				><option value="">All roots</option>{#each sortedRoots as [root, count] (root)}<option
						value={root}>{managementRootLabel(roots, root)} ({count.toLocaleString()})</option
					>{/each}</select
			></label
		>
		<label class="grid gap-1 text-xs"
			><span>Format</span><select class="select select-sm" bind:value={filters.audioFormat}
				><option value="">All formats</option
				>{#each sortedFormats as [format, count] (format)}<option value={format}
						>{format.toUpperCase()} ({count.toLocaleString()})</option
					>{/each}</select
			></label
		>
		<label class="grid gap-1 text-xs"
			><span>Change</span><select class="select select-sm" bind:value={filters.changeKind}
				><option value="">All changes</option><option value="tags"
					>Tags ({summary.tag_change_count.toLocaleString()})</option
				><option value="artwork">Artwork ({summary.artwork_change_count.toLocaleString()})</option
				><option value="path">Path ({summary.path_change_count.toLocaleString()})</option><option
					value="sidecars">Sidecars ({summary.sidecar_change_count.toLocaleString()})</option
				><option value="no_change">No change ({summary.no_change_count.toLocaleString()})</option
				></select
			></label
		>
	</div>
	<details class="mt-2 rounded-xl border border-base-content/10 bg-base-200/35 p-3">
		<summary class="cursor-pointer text-sm font-semibold"
			>Artist, release, collision, and preservation filters</summary
		>
		<div class="mt-3 grid gap-3 lg:grid-cols-2">
			<div class="space-y-2">
				<label class="grid gap-1 text-xs"
					><span>Find an artist or release</span><Input
						size="sm"
						label="Find an artist or release"
						bind:value={filters.catalogSearch}
						placeholder="Type at least two characters"
					/></label
				>
				{#if filters.artistId || filters.albumId}<div class="flex flex-wrap gap-1">
						{#if filters.artistId}<button
								type="button"
								class="badge badge-outline gap-1"
								onclick={() => filters.clearArtist()}>Artist: {filters.artistLabel} ×</button
							>{/if}{#if filters.albumId}<button
								type="button"
								class="badge badge-outline gap-1"
								onclick={() => filters.clearAlbum()}>Release: {filters.albumLabel} ×</button
							>{/if}
					</div>{/if}
				{#if filters.catalogSearch.trim().length >= 2}
					{#if catalogSearchQuery.isLoading}<div
							class="skeleton h-16 rounded-xl"
							role="status"
							aria-label="Searching artists and releases"
						></div>{:else if catalogSearchQuery.isError}<div
							class="alert alert-error py-2 text-xs"
							role="alert"
						>
							Could not search artists and releases.
						</div>{:else if catalogSearchQuery.data && catalogSearchQuery.data.artists.length + catalogSearchQuery.data.albums.length === 0}<p
							class="rounded-xl border border-dashed border-base-content/15 p-3 text-xs text-base-content/50"
						>
							No matching artists or releases.
						</p>{:else if catalogSearchQuery.data}<div
							class="grid max-h-44 gap-1 overflow-y-auto rounded-xl border border-base-content/10 bg-base-100 p-2"
						>
							{#each catalogSearchQuery.data.artists as artist (artist.id)}<Button
									variant="ghost"
									size="sm"
									class="justify-start"
									onclick={() => filters.chooseArtist(artist.id, artist.name)}
									>Artist · {artist.name}</Button
								>{/each}{#each catalogSearchQuery.data.albums as album (album.id)}<Button
									variant="ghost"
									size="sm"
									class="justify-start"
									onclick={() =>
										filters.chooseAlbum(album.id, `${album.artist_name} · ${album.title}`)}
									>Release · {album.artist_name} · {album.title}</Button
								>{/each}
						</div>{/if}
				{/if}
			</div>
			<div class="grid gap-2 sm:grid-cols-2">
				<label class="grid gap-1 text-xs sm:col-span-2"
					><span>Collision class</span><select
						class="select select-sm"
						bind:value={filters.collisionClass}
						><option value="">All collision classes</option
						>{#each collisionClasses as option (option)}<option value={option}
								>{titleManagementValue(option)}</option
							>{/each}</select
					></label
				>
				<label class="management-trigger"
					><input
						type="checkbox"
						class="checkbox checkbox-sm"
						bind:checked={filters.hasPreservedValue}
					/><span
						><strong>Preserved / local override</strong><small
							>Values intentionally left unchanged</small
						></span
					></label
				>
				<label class="management-trigger"
					><input
						type="checkbox"
						class="checkbox checkbox-sm"
						bind:checked={filters.hasRepresentationLoss}
					/><span
						><strong>Lossy representation</strong><small
							>Format cannot store the exact value shape</small
						></span
					></label
				>
			</div>
		</div>
	</details>
</Card>
