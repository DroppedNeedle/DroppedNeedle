<script lang="ts">
	import { onMount } from 'svelte';
	import { ListMusic, LoaderCircle, ChevronDown, CircleAlert } from 'lucide-svelte';
	import {
		chooseTrackHome,
		getTrackRequestSettings,
		requestPlaylistTracks,
		resolvePlaylistTracks
	} from '$lib/api/playlistTrackRequests';
	import type {
		CompilationPolicy,
		HomeOption,
		HomeTier,
		PlaylistDetail,
		PlaylistTrack,
		ResolutionSummary,
		TrackRequestJob,
		TrackRequestMode
	} from '$lib/api/playlists';
	import { muxEventStream, type MuxEventStream } from '$lib/queries/events/MuxEventStream';
	import { toastStore } from '$lib/stores/toast';
	import { authStore } from '$lib/stores/authStore.svelte';

	let {
		playlist,
		onrefresh,
		ontrackupdate,
		events = muxEventStream
	}: {
		playlist: PlaylistDetail;
		/** Re-read the playlist after a job stored new resolutions. */
		onrefresh: () => void;
		/** A flagged track's home was chosen; replace it in the local copy. */
		ontrackupdate: (track: PlaylistTrack) => void;
		events?: MuxEventStream;
	} = $props();

	const TIER_LABELS: Record<HomeTier, string> = {
		album: 'Studio albums',
		ep: 'EPs',
		single: 'Singles',
		soundtrack: 'Soundtracks',
		artist_compilation: "Artist's compilations",
		other_compilation: 'Other compilations',
		other: 'Other releases'
	};
	const POLICY_LABELS: Record<CompilationPolicy, string> = {
		match_playlist: 'Match playlist',
		studio: 'Studio version',
		flag: 'Flag for review'
	};
	const POLICY_HINTS: Record<CompilationPolicy, string> = {
		match_playlist: 'File each track under the album the playlist lists, compilations included',
		studio: "File each track under the artist's official album; compilations only as a last resort",
		flag: 'Where those two differ, hold the track so you can pick'
	};
	const OPTION_LABELS: Record<HomeOption['kind'], string> = {
		studio: 'Official',
		playlist: 'Playlist',
		compilation: 'Compilation'
	};

	let mode = $state<TrackRequestMode>('auto');
	let policy = $state<CompilationPolicy>('match_playlist');
	let pendingJob = $state<TrackRequestJob | null>(null);
	let summary = $state<ResolutionSummary | null>(null);
	let choosing = $state<string | null>(null);
	let reviewDialog = $state<HTMLDialogElement | null>(null);

	let working = $derived(pendingJob !== null);

	let missingTrackCount = $derived(
		playlist.tracks.filter(
			(t) => !t.library_file_id && (!t.available_sources || t.available_sources.length === 0)
		).length
	);
	let flaggedTracks = $derived(
		playlist.tracks.filter((t) => t.resolution_status === 'flagged' && t.flag_options?.length)
	);

	// The last choice sticks per user and browser; admin defaults apply otherwise.
	function choiceKey(): string {
		return `droppedneedle_track_request_choice_${authStore.user?.id ?? 'anon'}`;
	}

	function rememberChoice() {
		try {
			localStorage.setItem(choiceKey(), JSON.stringify({ mode, policy }));
		} catch {
			/* storage unavailable - defaults next time */
		}
	}

	function recalledChoice(): { mode?: TrackRequestMode; policy?: CompilationPolicy } | null {
		try {
			const raw = localStorage.getItem(choiceKey());
			return raw ? JSON.parse(raw) : null;
		} catch {
			return null;
		}
	}

	onMount(async () => {
		const recalled = recalledChoice();
		if (recalled?.mode && recalled?.policy) {
			mode = recalled.mode;
			policy = recalled.policy;
			return;
		}
		try {
			const defaults = await getTrackRequestSettings();
			mode = defaults.mode;
			policy = defaults.compilation_policy;
		} catch {
			/* keep the built-in defaults */
		}
	});

	function parse(event: Event): Record<string, unknown> | null {
		try {
			return JSON.parse((event as MessageEvent).data) as Record<string, unknown>;
		} catch {
			return null;
		}
	}

	/** Only the outcome of the job this panel started (the publisher replays
	 * the last event to reconnecting clients). */
	function isOurs(data: Record<string, unknown> | null): data is Record<string, unknown> {
		if (!data || !pendingJob || data.playlist_id !== playlist.id) return false;
		return pendingJob.job_id === null || data.job_id === pendingJob.job_id;
	}

	function handleResolved(event: Event) {
		const data = parse(event);
		if (!isOurs(data)) return;
		pendingJob = null;
		summary = (data.summary as ResolutionSummary) ?? null;
		onrefresh();
		reviewDialog?.showModal();
	}

	function handleRequested(event: Event) {
		const data = parse(event);
		if (!isOurs(data)) return;
		pendingJob = null;
		const result = (data.result ?? {}) as { message?: string; status?: string };
		const flagged = (data.summary as ResolutionSummary | undefined)?.flagged ?? 0;
		const base = result.message ?? 'Track requests submitted';
		const message = flagged
			? `${base}. ${flagged} track${flagged === 1 ? '' : 's'} need a choice`
			: base;
		toastStore.show({ message, type: result.status === 'failed' ? 'error' : 'success' });
		onrefresh();
	}

	function handleFailed(event: Event) {
		const data = parse(event);
		if (!isOurs(data)) return;
		pendingJob = null;
		toastStore.show({ message: "Couldn't finish this playlist's tracks", type: 'error' });
	}

	$effect(() => {
		const unsubs = [
			events.on('playlist_tracks_resolved', handleResolved),
			events.on('playlist_tracks_requested', handleRequested),
			events.on('playlist_tracks_failed', handleFailed)
		];
		return () => unsubs.forEach((unsub) => unsub());
	});

	async function start(run: () => Promise<TrackRequestJob>) {
		if (working) return;
		rememberChoice();
		try {
			const job = await run();
			pendingJob = job;
			if (job.status === 'already_running') {
				toastStore.show({ message: 'Already working on this playlist', type: 'info' });
			}
		} catch {
			pendingJob = null;
			toastStore.show({ message: "Couldn't start the track requests", type: 'error' });
		}
	}

	function handleRequestAll() {
		void start(() =>
			mode === 'auto'
				? requestPlaylistTracks(playlist.id, 'auto', policy)
				: resolvePlaylistTracks(playlist.id, policy)
		);
	}

	function confirmReview() {
		reviewDialog?.close();
		void start(() => requestPlaylistTracks(playlist.id, 'review'));
	}

	async function choose(track: PlaylistTrack, option: HomeOption) {
		if (choosing) return;
		choosing = track.id;
		try {
			ontrackupdate(await chooseTrackHome(playlist.id, track.id, option));
		} catch {
			toastStore.show({ message: "Couldn't save that choice", type: 'error' });
		} finally {
			choosing = null;
		}
	}

	let tierRows = $derived(
		summary
			? (Object.entries(summary.albums_by_tier) as [HomeTier, number][]).filter(([, n]) => n > 0)
			: []
	);
</script>

{#if missingTrackCount > 0 || flaggedTracks.length > 0}
	<div
		class="flex flex-wrap items-center gap-3 rounded-xl border border-base-300/40 bg-base-200/40 px-4 py-3"
	>
		<ListMusic class="h-4 w-4 shrink-0 text-base-content/50" />
		<p class="flex-1 text-sm text-base-content/70">
			{missingTrackCount}
			{missingTrackCount === 1 ? 'track' : 'tracks'} missing, requestable one by one
		</p>
		<div class="join">
			<button
				class="btn btn-accent btn-sm join-item"
				onclick={handleRequestAll}
				disabled={working || missingTrackCount === 0}
			>
				{#if working}
					<LoaderCircle class="h-3.5 w-3.5 animate-spin" />
					Working…
				{:else}
					<ListMusic class="h-3.5 w-3.5" />
					Request all tracks
				{/if}
			</button>
			<div class="dropdown dropdown-end">
				<button
					class="btn btn-accent btn-sm join-item px-2"
					aria-label="Request options"
					disabled={working}
				>
					<ChevronDown class="h-3.5 w-3.5" />
				</button>
				<div
					class="dropdown-content z-20 mt-1 w-72 rounded-box bg-base-200 p-3 shadow-lg"
					role="group"
					aria-label="Request options"
				>
					<fieldset class="mb-3">
						<legend class="mb-1 text-xs font-semibold uppercase text-base-content/50">Mode</legend>
						<label class="flex cursor-pointer items-start gap-2 py-1">
							<input type="radio" class="radio radio-sm" value="auto" bind:group={mode} />
							<span class="text-sm">
								Auto
								<span class="block text-xs text-base-content/50">Request straight away</span>
							</span>
						</label>
						<label class="flex cursor-pointer items-start gap-2 py-1">
							<input type="radio" class="radio radio-sm" value="review" bind:group={mode} />
							<span class="text-sm">
								Review
								<span class="block text-xs text-base-content/50">
									See which albums first, then confirm
								</span>
							</span>
						</label>
					</fieldset>
					<fieldset>
						<legend class="mb-1 text-xs font-semibold uppercase text-base-content/50">
							Where each track is filed
						</legend>
						{#each Object.keys(POLICY_LABELS) as CompilationPolicy[] as key (key)}
							<label class="flex cursor-pointer items-start gap-2 py-1">
								<input type="radio" class="radio radio-sm" value={key} bind:group={policy} />
								<span class="text-sm">
									{POLICY_LABELS[key]}
									<span class="block text-xs text-base-content/50">{POLICY_HINTS[key]}</span>
								</span>
							</label>
						{/each}
					</fieldset>
				</div>
			</div>
		</div>
	</div>
{/if}

{#if flaggedTracks.length > 0}
	<div class="rounded-xl border border-warning/30 bg-warning/5 px-4 py-3">
		<div class="mb-2 flex items-center gap-2 text-sm font-medium">
			<CircleAlert class="h-4 w-4 text-warning" />
			{flaggedTracks.length}
			{flaggedTracks.length === 1 ? 'track needs' : 'tracks need'} an album choice: the playlist's album
			and the official one differ
		</div>
		<ul class="space-y-2">
			{#each flaggedTracks as track (track.id)}
				<li class="flex flex-wrap items-center gap-2 text-sm">
					<span class="min-w-0 flex-1 truncate">
						{track.track_name}
						<span class="text-base-content/50">· {track.artist_name}</span>
					</span>
					{#each track.flag_options ?? [] as option (option.release_group_mbid)}
						<button
							class="btn btn-xs {option.kind === 'studio' ? 'btn-accent' : 'btn-ghost'}"
							disabled={choosing !== null}
							onclick={() => void choose(track, option)}
						>
							{OPTION_LABELS[option.kind] ?? option.kind}: {option.title}
						</button>
					{/each}
				</li>
			{/each}
		</ul>
	</div>
{/if}

<dialog class="modal" bind:this={reviewDialog} aria-labelledby="track-review-title">
	<div class="modal-box">
		<h3 id="track-review-title" class="text-lg font-bold">Request these tracks?</h3>
		{#if summary}
			<p class="py-2 text-sm text-base-content/70">
				{summary.resolved + summary.chosen}
				{summary.resolved + summary.chosen === 1 ? 'track' : 'tracks'} from {summary.albums}
				{summary.albums === 1 ? 'album' : 'albums'}
			</p>
			{#if tierRows.length > 0}
				<ul class="mb-2 space-y-1 text-sm">
					{#each tierRows as [tier, count] (tier)}
						<li class="flex justify-between">
							<span>{TIER_LABELS[tier]}</span>
							<span class="tabular-nums">{count}</span>
						</li>
					{/each}
				</ul>
			{/if}
			<ul class="space-y-1 text-xs text-base-content/60">
				{#if summary.owned}<li>{summary.owned} already in your library</li>{/if}
				{#if summary.flagged}<li>{summary.flagged} need you to pick an album</li>{/if}
				{#if summary.unresolved}<li>{summary.unresolved} couldn't be matched</li>{/if}
				{#if summary.failed}<li>{summary.failed} couldn't be looked up right now</li>{/if}
			</ul>
		{/if}
		<div class="modal-action">
			<button class="btn btn-ghost btn-sm" onclick={() => reviewDialog?.close()}>Cancel</button>
			<button
				class="btn btn-accent btn-sm"
				onclick={confirmReview}
				disabled={!summary || summary.resolved + summary.chosen === 0}
			>
				Request
			</button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop"><button>close</button></form>
</dialog>
