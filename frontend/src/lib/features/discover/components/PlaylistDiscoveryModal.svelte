<!--
	Album suggestions for one playlist, from its playlist page or from
	Discover (which first asks which playlist). The playlist it works from
	sits under the title; the results are album tiles with request, sample,
	go-to and ignore buttons, and a tile opens to list its tracks.
-->
<script lang="ts">
	import { X, Sparkles, ListMusic, RefreshCw, Check, ExternalLink } from 'lucide-svelte';
	import AlbumRequestButton from '$lib/features/requests/components/AlbumRequestButton.svelte';
	import GenreAlbumCard from '$lib/features/genre/components/GenreAlbumCard.svelte';
	import SampleButton from '$lib/features/discover/components/SampleButton.svelte';
	import { getDiscoverPlaylistSuggestionsV3Query } from '$lib/features/discover/queries/DiscoverV3Queries.svelte';
	import {
		getPlaylistListV3Query,
		isRedactedPlaylistV3
	} from '$lib/features/playlists/queries/PlaylistV3Queries.svelte';
	import type { components } from '$lib/api/v3/openapi';
	import { fetchAlbumTracks } from '$lib/features/album/queries/albumTracks';
	import { ApiError } from '$lib/api/errors';
	import { formatDuration, formatTotalDurationSec } from '$lib/utils/formatting';
	import type { Track } from '$lib/features/album/queries/types';
	import type { AlbumRef } from '$lib/features/discover/queries/types';
	import { albumRows } from '$lib/features/discover/components/sectionRows';
	import { toastStore } from '$lib/stores/toast';
	import { libraryStore } from '$lib/stores/library';
	import { albumHrefOrNull } from '$lib/utils/entityRoutes';
	import PlaylistMosaic from '$lib/media/PlaylistMosaic.svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import { untrack } from 'svelte';

	interface Props {
		open: boolean;
		playlistId?: string;
		playlistName?: string;
		/** The preselected playlist's mosaic. */
		playlistCoverUrls?: string[];
		playlistCustomCoverUrl?: string | null;
		/** The preselected playlist's counts line. */
		playlistMeta?: string;
	}

	let {
		open = $bindable(false),
		playlistId = '',
		playlistName = '',
		playlistCoverUrls = [],
		playlistCustomCoverUrl = null,
		playlistMeta = ''
	}: Props = $props();

	let selectedPlaylistId = $state('');
	let countValue = $state('15');
	let ignoredIds = $state(new Set<string>());
	let expandedAlbumId = $state<string | null>(null);
	let expandedTracks = $state<Track[]>([]);
	let expandLoading = $state(false);
	let expandAbortController = $state<AbortController | null>(null);

	const suggestionCount = $derived(parseInt(countValue, 10));
	const isPreselected = $derived(!!playlistId);
	const activePlaylistId = $derived(isPreselected ? playlistId : selectedPlaylistId);

	type PlaylistSummary = components['schemas']['PlaylistSummary'];
	const playlistListQuery = getPlaylistListV3Query(() => open && !isPreselected);
	const playlists = $derived(
		(playlistListQuery.data ?? []).filter(
			(p): p is PlaylistSummary => !isRedactedPlaylistV3(p) && 'is_owner' in p && p.is_owner
		)
	);
	const playlistsLoading = $derived(playlistListQuery.isLoading);
	const picked = $derived(playlists.find((p) => p.id === selectedPlaylistId) ?? null);

	const activePlaylist = $derived.by(() => {
		if (isPreselected) {
			return {
				name: playlistName,
				coverUrls: playlistCoverUrls,
				customCoverUrl: playlistCustomCoverUrl,
				meta: playlistMeta
			};
		}
		if (!picked) return null;
		const duration = picked.total_duration ? formatTotalDurationSec(picked.total_duration) : '';
		return {
			name: picked.name,
			coverUrls: picked.cover_urls,
			customCoverUrl: picked.custom_cover_url,
			meta: [`${picked.track_count} track${picked.track_count === 1 ? '' : 's'}`, duration]
				.filter(Boolean)
				.join(' · ')
		};
	});

	const suggestionsQuery = getDiscoverPlaylistSuggestionsV3Query(() => ({
		playlistId: activePlaylistId,
		count: suggestionCount,
		enabled: open
	}));

	const suggestions = $derived(suggestionsQuery.data?.suggestions ?? null);
	const profile = $derived(suggestionsQuery.data?.profile ?? null);
	const isLoading = $derived(suggestionsQuery.isLoading && !!activePlaylistId);
	const isError = $derived(suggestionsQuery.isError);
	const queryError = $derived(suggestionsQuery.error);

	const suggestedAlbums = $derived(
		suggestions && suggestions.type === 'albums' ? albumRows(suggestions) : []
	);
	const visibleAlbums = $derived(
		suggestedAlbums.filter((a) => !ignoredIds.has(a.musicbrainz_id ?? a.title))
	);

	const topGenres = $derived.by(() => {
		const distribution = profile?.genre_distribution ?? {};
		const entries = Object.entries(distribution);
		if (entries.length === 0) return [];
		entries.sort((a, b) => b[1].length - a[1].length);
		return entries.slice(0, 5).map(([genre]) => genre);
	});

	const errorStatus = $derived.by(() => {
		if (!queryError) return 0;
		if (queryError instanceof ApiError) return queryError.status;
		return 0;
	});

	$effect(() => {
		if (!open) untrack(resetState);
	});

	function resetState() {
		if (!isPreselected) selectedPlaylistId = '';
		ignoredIds = new Set();
		expandAbortController?.abort();
		expandAbortController = null;
		expandedAlbumId = null;
		expandedTracks = [];
	}

	function ignoreAlbum(album: AlbumRef) {
		const key = album.musicbrainz_id ?? album.title;
		ignoredIds = new Set([...ignoredIds, key]);
		if (expandedAlbumId === key) {
			expandedAlbumId = null;
			expandedTracks = [];
		}
	}

	async function toggleExpand(album: AlbumRef) {
		const key = album.musicbrainz_id ?? album.title;
		if (expandedAlbumId === key) {
			expandAbortController?.abort();
			expandAbortController = null;
			expandedAlbumId = null;
			expandedTracks = [];
			return;
		}

		expandAbortController?.abort();
		expandedAlbumId = key;
		expandedTracks = [];

		if (!album.musicbrainz_id) return;

		const controller = new AbortController();
		expandAbortController = controller;
		expandLoading = true;
		try {
			const data = await fetchAlbumTracks(album.musicbrainz_id, controller.signal);
			if (expandedAlbumId === key) {
				expandedTracks = data.tracks;
			}
		} catch (e) {
			if (controller.signal.aborted) return;
			toastStore.show({
				message: e instanceof Error ? e.message : 'Failed to load album tracks',
				type: 'error'
			});
			expandedTracks = [];
		} finally {
			if (!controller.signal.aborted) {
				expandLoading = false;
			}
		}
	}
	const countOptions = ['10', '15', '20', '30'].map((c) => ({ value: c, label: c }));
	const playlistOptions = $derived([
		{ value: '', label: 'Select a playlist', disabled: true },
		...playlists.map((pl) => ({
			value: pl.id,
			label: `${pl.name} (${pl.track_count} track${pl.track_count === 1 ? '' : 's'})`
		}))
	]);
	const showPicker = $derived(!isPreselected && !activePlaylistId);
	const showResults = $derived(!!activePlaylistId);
	const allIgnored = $derived(
		showResults &&
			!isLoading &&
			!isError &&
			suggestedAlbums.length > 0 &&
			visibleAlbums.length === 0
	);
</script>

<Modal bind:open size="sheet" icon={Sparkles} iconTone="action" title="Playlist suggestions">
	<div class="pl-sheet h-full overflow-y-auto px-4 py-5 md:px-6">
		{#if showPicker}
			<div class="pl-sheet__picker">
				<p class="text-muted">Pick a playlist to get album suggestions based on its contents.</p>
				<div class="pl-sheet__fields">
					<div class="min-w-0 flex-1">
						{#if playlistsLoading}
							<p class="pl-sheet__label">Playlist</p>
							<div class="flex items-center gap-2 py-2">
								<span class="loading loading-spinner loading-sm text-primary"></span>
								<span class="text-sm text-muted">Loading playlists…</span>
							</div>
						{:else if playlistListQuery.isError}
							<p class="pl-sheet__label">Playlist</p>
							<p class="py-2 text-sm text-error">
								{playlistListQuery.error instanceof Error
									? playlistListQuery.error.message
									: 'Failed to load playlists'}
							</p>
						{:else if playlists.length === 0}
							<p class="pl-sheet__label">Playlist</p>
							<p class="py-2 text-sm text-muted">No playlists found. Create one first.</p>
						{:else}
							<SelectField
								label="Playlist"
								options={playlistOptions}
								bind:value={selectedPlaylistId}
							/>
						{/if}
					</div>
					<div class="w-full sm:w-32">
						<SelectField label="Count" options={countOptions} bind:value={countValue} />
					</div>
				</div>
			</div>
		{/if}

		{#if showResults}
			{#if activePlaylist}
				<div class="pl-sheet__for">
					<div class="pl-sheet__mosaic">
						<PlaylistMosaic
							coverUrls={activePlaylist.coverUrls}
							customCoverUrl={activePlaylist.customCoverUrl}
							size="w-full h-full"
							rounded="rounded-none"
						/>
					</div>
					<div class="min-w-0 flex-1">
						<p class="pl-sheet__label">Suggestions for</p>
						<p class="pl-sheet__name">{activePlaylist.name}</p>
						{#if activePlaylist.meta}
							<p class="pl-sheet__meta">{activePlaylist.meta}</p>
						{/if}
					</div>
					<div class="w-28 shrink-0">
						<SelectField label="Results" size="sm" options={countOptions} bind:value={countValue} />
					</div>
				</div>
			{/if}

			<div aria-live="polite">
				{#if isLoading}
					<div class="flex flex-col gap-5">
						<div class="flex flex-wrap items-center gap-3">
							<div class="skeleton h-4 w-24 rounded"></div>
							<div class="skeleton h-4 w-20 rounded"></div>
							<div class="skeleton h-5 w-14 rounded-full"></div>
							<div class="skeleton h-5 w-16 rounded-full"></div>
						</div>
						<div class="grid grid-cols-2 gap-4 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5">
							{#each Array(8) as _, i (i)}
								<div class="flex flex-col gap-2">
									<div class="skeleton aspect-square w-full rounded-lg"></div>
									<div class="skeleton h-4 w-3/4 rounded"></div>
									<div class="skeleton h-3 w-1/2 rounded"></div>
								</div>
							{/each}
						</div>
					</div>
				{:else if isError}
					<div class="flex flex-col items-center justify-center gap-3 px-8 py-12 text-center">
						{#if errorStatus === 400}
							<ListMusic class="size-10 text-warning" aria-hidden="true" />
							<p class="max-w-md text-muted">{queryError?.message}</p>
						{:else}
							{#if errorStatus === 404}
								<p class="text-muted">Playlist not found.</p>
							{:else}
								<p class="text-muted">Couldn't load suggestions. Try again.</p>
							{/if}
							<Button
								variant="primary"
								size="display-sm"
								icon={RefreshCw}
								class="mt-2"
								onclick={() => suggestionsQuery.refetch()}
							>
								Try Again
							</Button>
						{/if}
					</div>
				{:else if allIgnored}
					<p class="px-8 py-12 text-center text-muted">
						No more suggestions left. Add more tracks or more varied artists for better results.
					</p>
				{:else if suggestions && visibleAlbums.length === 0}
					<p class="px-8 py-12 text-center text-muted">
						{suggestions.fallback_message ??
							'No suggestions yet - try adding more songs or more varied artists to generate recommendations.'}
					</p>
				{:else}
					{#if profile}
						<div class="pl-sheet__summary">
							<span>
								{profile.track_count ?? 0} track{(profile.track_count ?? 0) === 1 ? '' : 's'}
							</span>
							<span aria-hidden="true">·</span>
							<span>
								{(profile.artist_mbids ?? []).length} artist{(profile.artist_mbids ?? []).length ===
								1
									? ''
									: 's'}
							</span>
							{#if topGenres.length > 0}
								<span class="sr-only">Top genres:</span>
								<span class="flex flex-wrap gap-1.5">
									{#each topGenres as genre (genre)}
										<span class="pl-sheet__genre">{genre}</span>
									{/each}
								</span>
							{/if}
						</div>
					{/if}

					<div
						class="grid grid-cols-2 gap-x-4 gap-y-5 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5"
					>
						{#each visibleAlbums as album (album.musicbrainz_id ?? album.title)}
							{@const albumKey = album.musicbrainz_id ?? album.title}
							{@const isExpanded = expandedAlbumId === albumKey}
							<div
								class="flex flex-col"
								class:col-span-2={isExpanded}
								class:sm:col-span-3={isExpanded}
								class:md:col-span-4={isExpanded}
								class:lg:col-span-5={isExpanded}
							>
								<div class="flex flex-col gap-3 sm:flex-row">
									<div class="w-full" class:sm:w-44={isExpanded} class:sm:shrink-0={isExpanded}>
										<GenreAlbumCard {album} onclick={() => toggleExpand(album)} />
									</div>
									{#if isExpanded}
										<div class="pl-sheet__tracks">
											{#if expandLoading}
												<div class="flex flex-col gap-1.5 p-2">
													{#each Array(4) as _, i (i)}
														<div class="skeleton h-6 w-full rounded"></div>
													{/each}
												</div>
											{:else if expandedTracks.length > 0}
												<ol class="max-h-56 divide-y divide-line overflow-y-auto">
													{#each expandedTracks as track (track.position)}
														<li class="flex items-center gap-3 px-3 py-1.5 text-sm">
															<span class="w-6 text-right font-mono text-xs text-muted tabular-nums"
																>{track.position}</span
															>
															<span class="min-w-0 flex-1 truncate">{track.title}</span>
															<span class="font-mono text-xs text-muted tabular-nums"
																>{formatDuration(track.length)}</span
															>
															<SampleButton
																sampleKey={`track:${album.artist_name ?? ''}|${track.title}`}
																artist={album.artist_name ?? ''}
																title={track.title}
																kind="track"
																size="xs"
																albumMbid={album.musicbrainz_id}
																artistMbid={album.artist_id}
																coverUrl={album.cover_url}
															/>
														</li>
													{/each}
												</ol>
											{:else}
												<p class="p-3 text-xs text-muted">No track listing available.</p>
											{/if}
										</div>
									{/if}
								</div>
								<div class="mt-1 flex items-center gap-1">
									{#if album.musicbrainz_id}
										{@const albumLink = albumHrefOrNull(album.musicbrainz_id)}
										{@const isAlbumRequested =
											album.requested || libraryStore.isRequested(album.musicbrainz_id)}
										{#if !album.in_library && !isAlbumRequested}
											<div class="tooltip tooltip-bottom" data-tip="Request album">
												<AlbumRequestButton
													mbid={album.musicbrainz_id}
													artistName={album.artist_name ?? ''}
													albumName={album.title}
													artistMbid={album.artist_id ?? undefined}
												/>
											</div>
										{:else if album.in_library}
											<div class="tooltip tooltip-bottom" data-tip="In library">
												<span
													class="inline-flex size-9 items-center justify-center rounded-full border border-success/40 text-success"
													role="img"
													aria-label="In library"
												>
													<Check class="size-3.5" />
												</span>
											</div>
										{/if}

										<SampleButton
											sampleKey={album.musicbrainz_id}
											artist={album.artist_name ?? ''}
											title={album.title}
											kind="album"
											size="sm"
											artistMbid={album.artist_id}
											coverUrl={album.cover_url}
										/>

										{#if albumLink}
											<div class="tooltip tooltip-bottom" data-tip="Go to album">
												<IconButton
													size="sm"
													icon={ExternalLink}
													label="Go to {album.title}"
													href={albumLink}
													class="min-h-[36px] min-w-[36px]"
												/>
											</div>
										{/if}
									{/if}
									<IconButton
										size="sm"
										icon={X}
										label="Ignore suggestion"
										class="ml-auto min-h-[36px] min-w-[36px]"
										onclick={() => ignoreAlbum(album)}
									/>
								</div>
							</div>
						{/each}
					</div>
				{/if}
			</div>
		{/if}
	</div>
</Modal>

<style>
	/* labels read as mono capitals, like a sleeve's small print */
	.pl-sheet :global(.fieldset-legend),
	.pl-sheet__label {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}

	.pl-sheet__picker {
		display: flex;
		max-width: 44rem;
		flex-direction: column;
		gap: 1rem;
	}
	.pl-sheet__fields {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}
	@media (min-width: 640px) {
		.pl-sheet__fields {
			flex-direction: row;
			align-items: flex-start;
		}
	}

	/* the playlist the suggestions come from, ruled off from the results */
	.pl-sheet__for {
		display: flex;
		align-items: center;
		gap: 1rem;
		margin-bottom: 1.25rem;
		padding-bottom: 1.25rem;
		border-bottom: 1px solid var(--dn-line);
	}
	.pl-sheet__mosaic {
		flex: none;
		width: 4rem;
		aspect-ratio: 1;
		overflow: hidden;
		border-radius: 0.375rem;
	}
	.pl-sheet__name {
		overflow: hidden;
		font-family: var(--font-display);
		font-size: 1.5rem;
		line-height: 1.1;
		letter-spacing: 0.01em;
		text-overflow: ellipsis;
		text-transform: uppercase;
		white-space: nowrap;
		font-synthesis: none;
	}
	.pl-sheet__meta {
		margin-top: 0.125rem;
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.1em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}

	.pl-sheet__summary {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem 0.75rem;
		margin-bottom: 1.25rem;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}
	.pl-sheet__genre {
		padding: 0.125rem 0.5rem;
		border: 1px solid var(--dn-edge);
		border-radius: 9999px;
		font-family: var(--font-sans);
		font-size: 0.75rem;
		font-weight: 600;
		letter-spacing: 0.02em;
		text-transform: none;
	}

	.pl-sheet__tracks {
		min-width: 0;
		flex: 1;
		overflow: hidden;
		border: 1px solid var(--dn-line);
		border-radius: var(--radius-box);
		background-color: var(--color-base-100);
	}

	@media (max-width: 479px) {
		.pl-sheet__for {
			flex-wrap: wrap;
		}
		.pl-sheet__mosaic {
			width: 3.25rem;
		}
		.pl-sheet__name {
			font-size: 1.25rem;
		}
	}
</style>
