<script lang="ts">
	import { Radar, Search, X } from 'lucide-svelte';

	import { muxEventStream } from '$lib/queries/events/MuxEventStream';
	import {
		cancelSearch,
		dismissSearch,
		getSearchJobQuery,
		pickSearchCandidate,
		refreshSearchJob,
		type SearchCandidate
	} from '$lib/queries/downloads/SearchQueries.svelte';
	import { getPluginSourcesQuery } from '$lib/queries/plugins/PluginSourceQueries.svelte';

	import SearchResultCard from './SearchResultCard.svelte';

	let { jobId, onClose }: { jobId: string; onClose?: () => void } = $props();

	const jobQuery = getSearchJobQuery(() => jobId);
	const pick = pickSearchCandidate();
	const dismiss = dismissSearch();
	const cancel = cancelSearch();

	// The owner hears `search_job_updated` when the search finishes, fails or
	// is picked from; the job query refetches then.
	$effect(() => {
		const id = jobId;
		return muxEventStream.on('search_job_updated', (event) => {
			try {
				const data = JSON.parse((event as MessageEvent<string>).data) as { job_id?: string };
				if (data.job_id === id) refreshSearchJob(id);
			} catch {
				refreshSearchJob(id);
			}
		});
	});

	let showAll = $state(false);
	let pickingIndex = $state<number | null>(null);
	// A pick commits the download; keep every button locked afterwards so a
	// second click can't start a duplicate.
	let picked = $state(false);

	const job = $derived(jobQuery.data);
	const candidates = $derived(job?.candidates ?? []);
	const busy = $derived(picked || pickingIndex !== null || dismiss.isPending || cancel.isPending);

	// Sources are not comparable with each other, so each gets its own group,
	// ranked within. Plugin groups take the plugin's display name.
	const sourcesQuery = getPluginSourcesQuery();
	const sourceLabels = $derived(
		Object.fromEntries(
			(sourcesQuery.data?.sources ?? []).map((source) => [
				source.key.startsWith('plugin:') ? source.key : `plugin:${source.key}`,
				source.display_name || source.key
			])
		)
	);
	function labelFor(source: string): string {
		if (source === 'soulseek') return 'Soulseek';
		if (source === 'usenet') return 'Usenet';
		return sourceLabels[source] ?? source.replace(/^plugin:/, '');
	}
	const groups = $derived.by(() => {
		const bySource: { key: string; label: string; items: SearchCandidate[] }[] = [];
		for (const candidate of candidates) {
			let group = bySource.find((g) => g.key === candidate.source);
			if (!group) {
				group = { key: candidate.source, label: labelFor(candidate.source), items: [] };
				bySource.push(group);
			}
			group.items.push(candidate);
		}
		return bySource;
	});
	const hasMore = $derived(groups.some((g) => g.items.length > 3));

	function handlePick(index: number) {
		if (busy) return;
		pickingIndex = index;
		pick.mutate(
			{ jobId, candidate_index: index },
			{
				onSuccess: () => (picked = true),
				onError: () => (pickingIndex = null)
			}
		);
	}

	function handleDismiss() {
		if (busy) return;
		dismiss.mutate(jobId, { onSuccess: () => onClose?.() });
	}

	function handleClose() {
		if (job?.status === 'searching' || job?.status === 'completed' || job?.status === 'failed') {
			cancel.mutate(jobId, { onSuccess: () => onClose?.() });
		} else {
			onClose?.();
		}
	}
</script>

<section
	class="mt-3 space-y-3 border-t border-base-content/10 pt-3"
	aria-label="Manual search results"
>
	{#if jobQuery.isLoading}
		<div class="skeleton h-20 w-full rounded-box"></div>
	{:else if jobQuery.isError}
		<p class="text-sm text-error">Couldn't load this search. Try searching again.</p>
	{:else if job}
		{#if job.status === 'searching'}
			<div class="flex items-center gap-2 text-sm text-base-content/70">
				<span class="loading loading-spinner loading-sm"></span>
				Searching your download sources for {job.album_title}. This can take a minute or two.
			</div>
		{:else if job.status === 'matched'}
			<p class="text-sm text-success">
				Downloading your pick. Its progress shows in the queue above.
			</p>
		{:else if job.status === 'cancelled'}
			<p class="text-sm text-base-content/60">This search was closed.</p>
		{:else if job.reason}
			<div class="rounded-box border border-base-300 bg-base-100 px-3 py-2 text-sm">
				<p class="font-semibold">{job.reason.text}</p>
				<p class="text-base-content/65">{job.reason.action}</p>
			</div>
		{/if}

		{#if job.status === 'completed' && candidates.length > 0}
			<p class="text-xs text-base-content/50">
				{candidates.length}
				{candidates.length === 1 ? 'candidate' : 'candidates'}{job.tracks_total
					? `, compared with the ${job.tracks_total}-track edition`
					: ''}. Picking is safe: every file is checked before it reaches your library.
			</p>
			{#each groups as group (group.key)}
				{@const visible = showAll ? group.items : group.items.slice(0, 3)}
				<div class="space-y-2">
					<p class="text-xs font-semibold uppercase tracking-wide text-base-content/50">
						{group.label}
					</p>
					{#each visible as candidate (candidate.candidate_index)}
						<SearchResultCard
							{candidate}
							picking={pickingIndex === candidate.candidate_index}
							disabled={busy}
							onPick={() => handlePick(candidate.candidate_index)}
						/>
					{/each}
				</div>
			{/each}
		{/if}

		<div class="flex flex-wrap items-center justify-between gap-2">
			{#if hasMore && job.status === 'completed'}
				<button class="btn btn-ghost btn-xs" onclick={() => (showAll = !showAll)}>
					{showAll ? 'Show fewer' : `Show all ${candidates.length} candidates`}
				</button>
			{:else}
				<span></span>
			{/if}
			<div class="flex items-center gap-1">
				{#if job.status === 'completed' && job.release_group_mbid}
					<button
						class="btn btn-ghost btn-xs text-info"
						onclick={handleDismiss}
						disabled={busy}
						title="Turn all of these down and put the album on the watchlist. It is checked again on a schedule."
					>
						<Radar class="h-3.5 w-3.5" /> None of these - keep watching
					</button>
				{/if}
				<button class="btn btn-ghost btn-xs" onclick={handleClose} disabled={busy && !picked}>
					<X class="h-3.5 w-3.5" />
					{job.status === 'searching' ? 'Stop search' : 'Close'}
				</button>
			</div>
		</div>
	{:else}
		<p class="flex items-center gap-2 text-sm text-base-content/60">
			<Search class="h-4 w-4" /> No search to show.
		</p>
	{/if}
</section>
