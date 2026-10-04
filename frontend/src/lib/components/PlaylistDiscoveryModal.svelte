<script lang="ts">
	import {
		X,
		Sparkles,
		ListMusic,
		RefreshCw,
		Music2,
		Users,
		Tags,
		Check,
		ExternalLink
	} from 'lucide-svelte';
	import AlbumRequestButton from '$lib/components/AlbumRequestButton.svelte';
	import GenreAlbumCard from '$lib/components/GenreAlbumCard.svelte';
	import SampleButton from '$lib/components/discover/SampleButton.svelte';
	import { getDiscoverPlaylistSuggestionsV3Query } from '$lib/queries/discover/DiscoverV3Queries.svelte';
	import { toHomeSection } from '$lib/queries/discover/DiscoverV3Adapters';
	import { getPlaylistListQuery } from '$lib/queries/playlists/PlaylistQuery.svelte';
	import { isRedactedPlaylist, type PlaylistSummary } from '$lib/api/playlists';
	import { fetchAlbumTracks } from '$lib/api/albums';
	import { ApiError } from '$lib/api/client';
	import { formatDuration } from '$lib/utils/formatting';
	import type { HomeAlbum, Track } from '$lib/types';
	import { toastStore } from '$lib/stores/toast';
	import { integrationStore } from '$lib/stores/integration';
	import { libraryStore } from '$lib/stores/library';
	import { albumHrefOrNull } from '$lib/utils/entityRoutes';

	interface Props {
		open: boolean;
		playlistId?: string;
		playlistName?: string;
	}

	let { open = $bindable(false), playlistId = '', playlistName = '' }: Props = $props();

	let dialogEl: HTMLDialogElement | undefined = $state();
	let selectedPlaylistId = $state('');
	let selectedPlaylistName = $state('');
	let suggestionCount: number = $state(15);
	let ignoredIds = $state(new Set<string>());
	let expandedAlbumId = $state<string | null>(null);
	let expandedTracks = $state<Track[]>([]);
	let expandLoading = $state(false);
	let expandAbortController = $state<AbortController | null>(null);

	const isPreselected = $derived(!!playlistId);
	const activePlaylistId = $derived(isPreselected ? playlistId : selectedPlaylistId);
	const activePlaylistName = $derived(isPreselected ? playlistName : selectedPlaylistName);

	const playlistListQuery = getPlaylistListQuery(() => open && !isPreselected);
	const playlists = $derived(
		(playlistListQuery.data ?? []).filter(
			(p): p is PlaylistSummary => !isRedactedPlaylist(p) && p.is_owner
		)
	);
	const playlistsLoading = $derived(playlistListQuery.isLoading);

	const suggestionsQuery = getDiscoverPlaylistSuggestionsV3Query(() => ({
		playlistId: activePlaylistId,
		count: suggestionCount,
		enabled: open
	}));

	const suggestions = $derived(
		suggestionsQuery.data ? toHomeSection(suggestionsQuery.data.suggestions) : null
	);
	const profile = $derived(suggestionsQuery.data?.profile ?? null);
	const isLoading = $derived(suggestionsQuery.isLoading && !!activePlaylistId);
	const isError = $derived(suggestionsQuery.isError);
	const queryError = $derived(suggestionsQuery.error);

	const visibleAlbums = $derived.by(() => {
		if (!suggestions || suggestions.type !== 'albums') return [];
		return (suggestions.items as HomeAlbum[]).filter((a) => !ignoredIds.has(a.mbid ?? a.name));
	});

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
		if (!dialogEl) return;
		if (open) {
			dialogEl.showModal();
		} else {
			if (dialogEl.open) dialogEl.close();
			resetState();
		}
	});

	function resetState() {
		if (!isPreselected) {
			selectedPlaylistId = '';
			selectedPlaylistName = '';
		}
		ignoredIds = new Set();
		expandAbortController?.abort();
		expandAbortController = null;
		expandedAlbumId = null;
		expandedTracks = [];
	}

	function handleClose() {
		open = false;
	}

	function handlePlaylistSelect(e: Event) {
		const target = e.target as HTMLSelectElement;
		const id = target.value;
		selectedPlaylistId = id;
		const found = playlists.find((p) => p.id === id);
		selectedPlaylistName = found?.name ?? '';
	}

	function handleCountChange(e: Event) {
		const target = e.target as HTMLSelectElement;
		suggestionCount = parseInt(target.value, 10);
	}

	function ignoreAlbum(album: HomeAlbum) {
		const key = album.mbid ?? album.name;
		ignoredIds = new Set([...ignoredIds, key]);
		if (expandedAlbumId === key) {
			expandedAlbumId = null;
			expandedTracks = [];
		}
	}

	async function toggleExpand(album: HomeAlbum) {
		const key = album.mbid ?? album.name;
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

		if (!album.mbid) return;

		const controller = new AbortController();
		expandAbortController = controller;
		expandLoading = true;
		try {
			const data = await fetchAlbumTracks(album.mbid, controller.signal);
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
	const countOptions = [10, 15, 20, 30] as const;
	const showPicker = $derived(!isPreselected && !activePlaylistId);
	const showResults = $derived(!!activePlaylistId);
	const allIgnored = $derived(showResults && !isLoading && !isError && visibleAlbums.length === 0);
</script>

<dialog bind:this={dialogEl} class="modal" onclose={handleClose} aria-label="Playlist Discovery">
	<div
		class="modal-box w-[92vw] max-w-4xl max-h-[85vh] sm:max-w-4xl max-sm:w-screen max-sm:max-w-full max-sm:max-h-screen max-sm:rounded-none flex flex-col p-0! overflow-hidden rounded-2xl bg-base-100/80 backdrop-blur-xl shadow-[0_8px_64px_oklch(from_var(--color-primary)_l_c_h_/_0.12)] border border-white/10 relative"
	>
		<div
			class="absolute top-0 inset-x-0 h-1 bg-linear-to-r from-primary/80 via-secondary/60 to-primary/80 shadow-[0_2px_12px_oklch(from_var(--color-primary)_l_c_h_/_0.3)] z-10 rounded-t-2xl"
		></div>

		<div class="flex items-center justify-between px-6 pt-6 pb-4 shrink-0 border-b border-white/5">
			<div class="flex items-center gap-2 min-w-0">
				<Sparkles class="h-5 w-5 text-primary shrink-0" />
				<h2 class="text-lg font-bold truncate">
					{#if activePlaylistName}
						Suggestions for {activePlaylistName}
					{:else}
						Playlist Discovery
					{/if}
				</h2>
			</div>
			<button
				type="button"
				class="btn btn-ghost btn-circle min-h-[44px] min-w-[44px]"
				onclick={handleClose}
				aria-label="Close"
			>
				<X class="h-5 w-5" />
			</button>
		</div>

		<div class="flex-1 overflow-y-auto px-6 pb-6">
			{#if showPicker}
				<div
					class="flex flex-col gap-4 py-4 rounded-xl bg-base-200/30 p-4 mt-4 border border-white/5"
				>
					<p class="text-sm text-base-content/60">
						Pick a playlist to get album suggestions based on its contents.
					</p>

					<div class="flex flex-col sm:flex-row gap-3">
						<div class="flex-1">
							<label class="label" for="playlist-select">
								<span class="label-text font-medium">Playlist</span>
							</label>
							{#if playlistsLoading}
								<div class="flex items-center gap-2 py-2">
									<span class="loading loading-spinner loading-sm text-primary"></span>
									<span class="text-sm text-base-content/60">Loading playlists…</span>
								</div>
							{:else if playlistListQuery.isError}
								<p class="text-sm text-error/70 py-2">
									{playlistListQuery.error instanceof Error
										? playlistListQuery.error.message
										: 'Failed to load playlists'}
								</p>
							{:else if playlists.length === 0}
								<p class="text-sm text-base-content/50 py-2">
									No playlists found. Create one first.
								</p>
							{:else}
								<select
									id="playlist-select"
									class="select select-bordered w-full"
									value=""
									onchange={handlePlaylistSelect}
								>
									<option value="" disabled>Select a playlist</option>
									{#each playlists as pl (pl.id)}
										<option value={pl.id}>
											{pl.name} ({pl.track_count} track{pl.track_count === 1 ? '' : 's'})
										</option>
									{/each}
								</select>
							{/if}
						</div>

						<div class="w-full sm:w-28">
							<label class="label" for="count-select">
								<span class="label-text font-medium">Count</span>
							</label>
							<select
								id="count-select"
								class="select select-bordered w-full"
								value={String(suggestionCount)}
								onchange={handleCountChange}
							>
								{#each countOptions as c (c)}
									<option value={String(c)}>{c}</option>
								{/each}
							</select>
						</div>
					</div>
				</div>
			{/if}

			{#if showResults}
				<div aria-live="polite">
					{#if !showPicker}
						<div class="flex items-center gap-2 mb-4 mt-4">
							<label class="text-sm text-base-content/60" for="count-select-inline">Results:</label>
							<select
								id="count-select-inline"
								class="select select-bordered select-sm w-20"
								value={String(suggestionCount)}
								onchange={handleCountChange}
							>
								{#each countOptions as c (c)}
									<option value={String(c)}>{c}</option>
								{/each}
							</select>
						</div>
					{/if}

					{#if isLoading}
						<div class="flex flex-col gap-5 py-4">
							<div
								class="rounded-xl bg-gradient-to-r from-primary/10 via-base-200/60 to-secondary/10 p-4 border border-white/10 backdrop-blur-sm"
							>
								<div class="flex flex-wrap items-center gap-4">
									<div class="skeleton h-4 w-24 rounded"></div>
									<div class="skeleton h-4 w-20 rounded"></div>
									<div class="flex gap-1">
										<div class="skeleton h-5 w-12 rounded-full"></div>
										<div class="skeleton h-5 w-16 rounded-full"></div>
										<div class="skeleton h-5 w-10 rounded-full"></div>
									</div>
								</div>
							</div>
							<div class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 gap-3">
								{#each Array(8) as _, i (i)}
									<div class="flex flex-col gap-2">
										<div class="skeleton aspect-square w-full rounded-2xl"></div>
										<div class="skeleton h-4 w-3/4 rounded"></div>
										<div class="skeleton h-3 w-1/2 rounded"></div>
									</div>
								{/each}
							</div>
						</div>
					{:else if isError}
						<div class="flex flex-col items-center justify-center py-12 px-8">
							{#if errorStatus === 422}
								<ListMusic class="h-10 w-10 text-warning mb-3" />
								<p class="text-center text-base-content/70 max-w-md">
									This playlist doesn't have enough artist data for discovery. Add more tracks with
									known artists and try again.
								</p>
							{:else if errorStatus === 404}
								<p class="text-center text-base-content/70">Playlist not found.</p>
							{:else}
								<p class="text-center text-base-content/70 mb-4">
									Couldn't load suggestions. Try again.
								</p>
							{/if}
							<button
								type="button"
								class="btn btn-primary btn-sm mt-4"
								onclick={() => suggestionsQuery.refetch()}
							>
								<RefreshCw class="h-3.5 w-3.5" />
								Try Again
							</button>
						</div>
					{:else if allIgnored}
						<div class="flex flex-col items-center justify-center py-12 px-8">
							<p class="text-center text-base-content/60">
								No more suggestions left. Add more tracks or more varied artists for better results.
							</p>
						</div>
					{:else if suggestions && visibleAlbums.length === 0}
						<div class="flex flex-col items-center justify-center py-12 px-8">
							<p class="text-center text-base-content/60">
								{suggestions.fallback_message ??
									'No suggestions yet - try adding more songs or more varied artists to generate recommendations.'}
							</p>
						</div>
					{:else}
						{#if profile}
							<div
								class="rounded-xl bg-gradient-to-r from-primary/10 via-base-200/60 to-secondary/10 p-4 mb-5 border border-white/10 backdrop-blur-sm"
							>
								<div class="flex flex-wrap items-center gap-4 text-sm">
									<span class="inline-flex items-center gap-1.5 text-base-content/70">
										<Music2 class="h-3.5 w-3.5" />
										{profile.track_count ?? 0} track{(profile.track_count ?? 0) === 1 ? '' : 's'}
									</span>
									<span class="inline-flex items-center gap-1.5 text-base-content/70">
										<Users class="h-3.5 w-3.5" />
										{(profile.artist_mbids ?? []).length} artist{(profile.artist_mbids ?? [])
											.length === 1
											? ''
											: 's'}
									</span>
									{#if topGenres.length > 0}
										<span class="inline-flex items-center gap-1.5 text-base-content/70">
											<Tags class="h-3.5 w-3.5 shrink-0" />
										</span>
										<div class="flex flex-wrap gap-1">
											{#each topGenres as genre (genre)}
												<span
													class="inline-flex items-center rounded-full bg-gradient-to-r from-primary/20 to-secondary/20 px-2.5 py-0.5 text-xs font-medium text-primary border border-primary/20"
													>{genre}</span
												>
											{/each}
										</div>
									{/if}
								</div>
							</div>
						{/if}

						<div class="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 gap-3">
							{#each visibleAlbums as album (album.mbid ?? album.name)}
								{@const albumKey = album.mbid ?? album.name}
								{@const isExpanded = expandedAlbumId === albumKey}
								<div
									class="flex flex-col overflow-hidden"
									class:col-span-2={isExpanded}
									class:sm:col-span-3={isExpanded}
									class:md:col-span-4={isExpanded}
								>
									<div class="flex flex-col sm:flex-row gap-2">
										<div class="w-full" class:sm:w-40={isExpanded} class:sm:shrink-0={isExpanded}>
											<GenreAlbumCard {album} onclick={() => toggleExpand(album)} />
										</div>
										{#if isExpanded}
											<div
												class="flex-1 min-w-0 rounded-lg bg-base-200/30 border border-white/5 p-2 mt-2 sm:mt-0"
											>
												{#if expandLoading}
													<div class="flex flex-col gap-1.5 py-1">
														{#each Array(4) as _, i (i)}
															<div class="skeleton h-6 w-full rounded"></div>
														{/each}
													</div>
												{:else if expandedTracks.length > 0}
													<div class="max-h-48 overflow-y-auto">
														<table class="table table-xs w-full">
															<tbody>
																{#each expandedTracks as track (track.position)}
																	<tr class="hover:bg-base-200/50">
																		<td class="opacity-70 w-6 text-right pr-2 text-base-content/40"
																			>{track.position}</td
																		>
																		<td class="truncate max-w-0">{track.title}</td>
																		<td class="text-base-content/40 text-right w-12 shrink-0"
																			>{formatDuration(track.length)}</td
																		>
																		<td class="w-8 px-0">
																			<SampleButton
																				sampleKey={`track:${album.artist_name ?? ''}|${track.title}`}
																				artist={album.artist_name ?? ''}
																				title={track.title}
																				kind="track"
																				size="xs"
																				albumMbid={album.mbid}
																				artistMbid={album.artist_mbid}
																				coverUrl={album.image_url}
																			/>
																		</td>
																	</tr>
																{/each}
															</tbody>
														</table>
													</div>
												{:else}
													<p class="text-xs text-base-content/50 py-2">
														No track listing available.
													</p>
												{/if}
											</div>
										{/if}
									</div>
									<div class="flex items-center justify-center gap-1 mt-2">
										{#if album.mbid}
											{@const albumLink = albumHrefOrNull(album.mbid)}
											{@const isAlbumRequested =
												album.requested || libraryStore.isRequested(album.mbid)}
											{#if albumLink}
												<div class="tooltip tooltip-bottom" data-tip="Go to album">
													<a
														href={albumLink}
														class="btn btn-circle btn-ghost btn-sm border border-white/10 hover:border-primary/30 hover:text-primary min-h-[36px] min-w-[36px]"
														aria-label="Go to {album.name}"
													>
														<ExternalLink class="h-3.5 w-3.5" />
													</a>
												</div>
											{/if}

											{#if $integrationStore.download_client && !album.in_library && !isAlbumRequested}
												<div class="tooltip tooltip-bottom" data-tip="Request album">
													<AlbumRequestButton
														mbid={album.mbid}
														artistName={album.artist_name ?? ''}
														albumName={album.name}
														artistMbid={album.artist_mbid ?? undefined}
													/>
												</div>
											{:else if album.in_library}
												<div class="tooltip tooltip-bottom" data-tip="In library">
													<span
														class="btn btn-circle btn-ghost btn-sm border border-success/30 text-success min-h-[36px] min-w-[36px] cursor-default"
													>
														<Check class="h-3.5 w-3.5" />
													</span>
												</div>
											{/if}

											{#if album.mbid}
												<SampleButton
													sampleKey={album.mbid}
													artist={album.artist_name ?? ''}
													title={album.name}
													kind="album"
													size="sm"
													artistMbid={album.artist_mbid}
													coverUrl={album.image_url}
												/>
											{/if}
										{/if}
										<button
											type="button"
											class="btn btn-circle btn-ghost btn-sm border border-white/10 hover:border-error/30 hover:text-error min-h-[36px] min-w-[36px]"
											onclick={() => ignoreAlbum(album)}
											title="Ignore suggestion"
											aria-label="Ignore suggestion"
										>
											<X class="h-3.5 w-3.5" />
										</button>
									</div>
								</div>
							{/each}
						</div>
					{/if}
				</div>
			{/if}
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button type="submit">close</button>
	</form>
</dialog>
