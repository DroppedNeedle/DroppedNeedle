<script lang="ts">
	import { Radar } from 'lucide-svelte';

	import { cancelDownload } from '$lib/queries/downloads/DownloadMutations.svelte';
	import {
		dismissReview,
		getSearchJobQuery,
		pickSearchCandidate
	} from '$lib/queries/downloads/SearchQueries.svelte';
	import type { DownloadTask } from '$lib/types';

	import SearchResultCard from './SearchResultCard.svelte';

	let { task }: { task: DownloadTask } = $props();

	const jobQuery = getSearchJobQuery(() => task.search_job_id ?? '');
	const pick = pickSearchCandidate();
	const cancel = cancelDownload();
	const dismiss = dismissReview();

	let showAll = $state(false);
	let pickingIndex = $state<number | null>(null);
	// Once a pick succeeds the task is committed but this card lingers until the next poll
	// moves it out of Review - keep every pick locked in that window so a second click can't
	// fire a duplicate download.
	let picked = $state(false);
	// mirrors `picked` for the "None of these" path: locked once the album moved
	// to the watchlist, until the next poll drops the card
	let dismissed = $state(false);

	function handleDismiss() {
		if (!task.search_job_id || dismissed || picked) return;
		dismiss.mutate(task.search_job_id, { onSuccess: () => (dismissed = true) });
	}

	const candidates = $derived(jobQuery.data?.candidates ?? []);
	// Keep each candidate's ORIGINAL index - the backend's candidate_index is into the
	// full pooled list, so grouping/truncating must not renumber.
	const indexed = $derived(candidates.map((candidate, index) => ({ candidate, index })));
	// Source-grouped (D16): Soulseek and Usenet scores aren't commensurable, so they show
	// in separate labelled groups, ranked within each - never interleaved.
	const groups = $derived(
		[
			{
				key: 'soulseek',
				label: 'Soulseek',
				items: indexed.filter((c) => (c.candidate.source ?? 'soulseek') === 'soulseek')
			},
			{
				key: 'usenet',
				label: 'Usenet',
				items: indexed.filter((c) => c.candidate.source === 'usenet')
			}
		].filter((g) => g.items.length > 0)
	);
	const isTrack = $derived(task.download_type === 'track');

	function handlePick(index: number) {
		if (!task.search_job_id || picked || pickingIndex !== null) return;
		pickingIndex = index;
		pick.mutate(
			{ jobId: task.search_job_id, candidate_index: index },
			{
				onSuccess: () => (picked = true),
				// only re-open on failure; a success stays locked until the card unmounts
				onError: () => (pickingIndex = null)
			}
		);
	}
</script>

<div class="mt-3 space-y-4 border-t border-base-content/10 pt-3">
	{#if jobQuery.isLoading}
		<div class="skeleton h-20 w-full rounded-box"></div>
	{:else if candidates.length === 0}
		<p class="text-sm text-base-content/60">No candidates available to review.</p>
	{:else}
		<p class="text-xs text-base-content/40">
			Not sure? Picking is safe - every file is verified before it reaches your library, and
			anything that can't be verified is held for you to listen to first.
		</p>
		{#each groups as group (group.key)}
			{@const visible = showAll ? group.items : group.items.slice(0, 3)}
			<div class="space-y-2">
				<p class="text-xs font-semibold uppercase tracking-wide text-base-content/50">
					{group.label}
				</p>
				{#each visible as { candidate, index } (index)}
					<SearchResultCard
						{candidate}
						albumTitle={task.album_title}
						viaAlbumNzb={group.key === 'usenet' && isTrack}
						picking={pickingIndex === index}
						disabled={picked || pickingIndex !== null}
						onPick={() => handlePick(index)}
					/>
				{/each}
			</div>
		{/each}
		<div class="flex items-center justify-between gap-2">
			{#if groups.some((g) => g.items.length > 3)}
				<button class="btn btn-ghost btn-xs" onclick={() => (showAll = !showAll)}>
					{showAll ? 'Show fewer' : `Show all ${candidates.length} candidates`}
				</button>
			{:else}
				<span></span>
			{/if}
			<div class="flex items-center gap-1">
				<button
					class="btn btn-ghost btn-xs text-info"
					onclick={handleDismiss}
					disabled={dismissed || picked || dismiss.isPending}
					title="Reject all of these and put the album on the watchlist - it'll be re-checked on a schedule"
				>
					<Radar class="h-3.5 w-3.5" />
					{dismissed ? 'On the watchlist' : 'None of these - keep watching'}
				</button>
				<button
					class="btn btn-ghost btn-xs text-error"
					onclick={() => cancel.mutate(task.id)}
					disabled={dismissed || cancel.isPending}
					title="Give up on this album entirely - it won't be watched"
				>
					Cancel request
				</button>
			</div>
		</div>
	{/if}
</div>
