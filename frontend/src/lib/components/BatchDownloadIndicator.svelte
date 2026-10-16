<script lang="ts">
	import { Download, Check, Minus, ChevronUp, X } from 'lucide-svelte';
	import { batchDownloadStore } from '$lib/stores/batchDownloadStatus.svelte';
	import { libraryStore } from '$lib/stores/library';
	import { playerStore } from '$lib/stores/player.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	let completedPerJob = $derived.by(() => {
		const { mbidSet } = $libraryStore;
		const counts: Record<string, number> = {};
		for (const job of batchDownloadStore.jobs) {
			let done = 0;
			for (const mbid of job.musicbrainzIds) {
				if (mbidSet.has(mbid.toLowerCase())) done++;
			}
			counts[job.artistId] = done;
		}
		return counts;
	});

	let allComplete = $derived.by(() =>
		batchDownloadStore.jobs.every((job) => (completedPerJob[job.artistId] ?? 0) >= job.total)
	);

	$effect(() => {
		if (allComplete && batchDownloadStore.jobs.length > 0) {
			const timer = setTimeout(() => {
				batchDownloadStore.clear();
			}, 8000);
			return () => clearTimeout(timer);
		}
	});
</script>

{#if batchDownloadStore.hasActive}
	<div
		class="fixed right-4 z-40 w-80 max-w-[calc(100vw-2rem)]"
		class:bottom-24={playerStore.isPlayerVisible}
		class:bottom-4={!playerStore.isPlayerVisible}
		role="status"
		aria-live="polite"
		aria-label="Batch download progress"
	>
		{#if batchDownloadStore.minimized}
			<button
				class="flex items-center gap-2 bg-base-200 rounded-full shadow-xl border border-base-300 pl-3 pr-2 py-1.5 w-auto max-w-full cursor-pointer hover:bg-base-300/80 transition-colors animate-slide-in-right ml-auto"
				onclick={() => batchDownloadStore.toggleMinimized()}
				aria-label="Expand download progress"
			>
				<div class="shrink-0">
					{#if allComplete}
						<Check class="h-3.5 w-3.5 text-success" />
					{:else}
						<Download class="h-3.5 w-3.5 text-accent animate-pulse" />
					{/if}
				</div>
				<span class="text-xs font-medium truncate" class:text-success={allComplete}>
					{#if allComplete}
						Downloads Complete
					{:else}
						Downloading ({batchDownloadStore.jobs.length} artist{batchDownloadStore.jobs.length !==
						1
							? 's'
							: ''})
					{/if}
				</span>
				<ChevronUp class="h-3.5 w-3.5 opacity-50 shrink-0" />
			</button>
		{:else}
			<Card variant="bordered" padding="md" class="shadow-xl animate-slide-in-right">
				<div class="flex items-center justify-between gap-2 mb-3">
					<div class="flex items-center gap-2.5">
						<div
							class="rounded-full p-1.5 shrink-0 {allComplete ? 'bg-success/15' : 'bg-accent/10'}"
						>
							{#if allComplete}
								<Check class="h-4 w-4 text-success" />
							{:else}
								<Download class="h-4 w-4 text-accent animate-pulse" />
							{/if}
						</div>
						<span class="font-semibold text-sm" class:text-success={allComplete}>
							{allComplete ? 'Downloads Complete' : 'Downloading Discography'}
						</span>
					</div>
					<div class="flex items-center gap-0.5 shrink-0">
						<IconButton
							size="xs"
							icon={Minus}
							label="Minimize"
							onclick={() => batchDownloadStore.toggleMinimized()}
						/>
						<IconButton
							size="xs"
							icon={X}
							label="Dismiss"
							onclick={() => batchDownloadStore.clear()}
						/>
					</div>
				</div>

				{#each batchDownloadStore.jobs as job (job.artistId)}
					{@const completed = completedPerJob[job.artistId] ?? 0}
					{@const progress = job.total > 0 ? Math.round((completed / job.total) * 100) : 0}
					{@const jobDone = completed >= job.total}
					<div class="mb-2 last:mb-0">
						<div class="flex items-center justify-between text-xs mb-1">
							<span class="font-medium truncate mr-2">{job.artistName}</span>
							<span class="text-base-content/50 shrink-0">
								{completed}/{job.total}
							</span>
						</div>
						<div
							class="w-full bg-base-300 rounded-full h-1.5 overflow-hidden"
							role="progressbar"
							aria-valuenow={progress}
							aria-valuemin={0}
							aria-valuemax={100}
						>
							<div
								class="h-full rounded-full transition-all duration-700 ease-out"
								class:bg-accent={!jobDone}
								class:bg-success={jobDone}
								style="width: {progress}%"
							></div>
						</div>
					</div>
				{/each}
			</Card>
		{/if}
	</div>
{/if}

<style>
	@keyframes slide-in-right {
		from {
			opacity: 0;
			transform: translateX(110%);
		}
		to {
			opacity: 1;
			transform: translateX(0);
		}
	}
	.animate-slide-in-right {
		animation: slide-in-right 0.35s cubic-bezier(0.16, 1, 0.3, 1);
	}
</style>
