<script lang="ts">
	import { FileDown, RotateCcw, TimerOff } from 'lucide-svelte';

	import AlbumImage from '$lib/components/AlbumImage.svelte';
	import {
		reimportDownload,
		retryDownload,
		stopAutoRetry
	} from '$lib/queries/downloads/DownloadMutations.svelte';
	import {
		canReimport,
		formatCountdown,
		retryLadderState
	} from '$lib/queries/downloads/downloadStatus';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { nowSeconds, startSharedClock } from '$lib/stores/clock.svelte';
	import type { DownloadTask } from '$lib/types';
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';
	import { formatRelativeTime } from '$lib/utils/formatting';

	import CueCountdown from './CueCountdown.svelte';

	let { task }: { task: DownloadTask } = $props();

	const retry = retryDownload();
	const stopRetry = stopAutoRetry();
	const reimport = reimportDownload();

	// this card owns its clock dependency: the shared tick runs while any Wanted card is
	// mounted (refcounted -> still one interval), so the countdown ticks without relying on
	// a parent to start it
	$effect(() => startSharedClock());

	// recomputes every tick because retryLadderState reads the shared clock
	const ladder = $derived(retryLadderState(task, nowSeconds()));
	const countdown = $derived(ladder ? formatCountdown(ladder.secondsUntilNext) : '');
	const urgent = $derived(!!ladder && ladder.secondsUntilNext <= 60);
	const lastTried = $derived(
		task.completed_at ? formatRelativeTime(new Date(task.completed_at * 1000)) : null
	);
	const isPartial = $derived(task.status === 'partial');

	function rungLabel(minutes: number): string {
		return minutes < 60 ? `${minutes}m` : `${Math.round(minutes / 60)}h`;
	}

	let retried = $state(false);
	function onRetryNow() {
		retried = true;
		retry.mutate(task.id, { onError: () => (retried = false) });
	}
</script>

<article
	class="wanted-card rounded-2xl border border-warning/20 bg-base-200/50 p-3 backdrop-blur-sm sm:p-4"
>
	<div class="flex items-start gap-3 sm:gap-4">
		<div class="size-14 shrink-0 overflow-hidden rounded-lg ring-1 ring-base-content/10 sm:size-16">
			<AlbumImage
				mbid={task.release_group_mbid}
				alt={task.album_title}
				size="sm"
				rounded="lg"
				className="h-full w-full"
			/>
		</div>

		<div class="min-w-0 flex-1">
			<h3 class="truncate text-sm font-semibold sm:text-base">
				<a href={albumHref(task.release_group_mbid)} class="hover:text-primary transition-colors">
					{task.album_title}
				</a>
			</h3>
			<p class="truncate text-xs text-base-content/60">
				{#if task.artist_mbid}
					<a href={artistHref(task.artist_mbid)} class="hover:text-primary transition-colors">
						{task.artist_name}
					</a>
				{:else}
					{task.artist_name}
				{/if}
				{#if task.year}<span class="text-base-content/30"> · </span>{task.year}{/if}
				{#if task.download_type === 'track' && task.track_title}<span class="text-base-content/30">
						·
					</span>{task.track_title}{/if}
			</p>

			<p class="mt-1.5 text-[11px] font-bold uppercase tracking-[0.14em] text-warning">
				{#if ladder}Next spin · retry {ladder.attempt} of {ladder.total}{:else}Retrying{/if}
			</p>

			{#if ladder}
				<div class="mt-1.5 flex flex-wrap items-center gap-1" aria-label="Retry schedule">
					{#each ladder.rungs as min, i (i)}
						<span
							class="rung"
							class:rung-done={i < ladder.index}
							class:rung-now={i === ladder.index}
						>
							{rungLabel(min)}
						</span>
					{/each}
				</div>
			{/if}

			<p class="mt-1.5 text-xs text-base-content/55">
				{#if isPartial && task.files_total > 0}
					Got {task.files_completed}/{task.files_total} tracks{#if lastTried}
						· last tried {lastTried}{/if}
				{:else if lastTried}
					Last tried {lastTried}
				{/if}
			</p>
			{#if task.error_message}
				<p class="mt-0.5 line-clamp-2 text-xs text-base-content/45">{task.error_message}</p>
			{/if}
		</div>

		<div class="flex shrink-0 flex-col items-center gap-2">
			<CueCountdown fraction={ladder?.fractionElapsed ?? 0} label={countdown} {urgent} size={68} />
			<div class="flex flex-col items-stretch gap-1">
				<button
					class="btn btn-ghost btn-primary btn-xs"
					onclick={onRetryNow}
					disabled={retry.isPending || retried}
					title="Try this download again right now"
				>
					<RotateCcw class="h-3.5 w-3.5" />
					{retried ? 'Retrying…' : 'Retry now'}
				</button>
				<button
					class="btn btn-ghost btn-xs text-base-content/60 hover:text-error"
					onclick={() => stopRetry.mutate(task.id)}
					disabled={stopRetry.isPending}
					title="Stop hunting for this - it won't be watched for later either"
				>
					<TimerOff class="h-3.5 w-3.5" /> Stop
				</button>
			</div>
		</div>
	</div>
	{#if authStore.isAdmin && canReimport(task)}
		<div class="mt-2 flex justify-end">
			<button
				class="btn btn-ghost btn-xs"
				onclick={() =>
					reimport.mutate({ id: task.id, release_group_mbid: task.release_group_mbid })}
				disabled={reimport.isPending}
				title="Already fixed this in slskd? Check the downloads mount again without re-searching."
				aria-label="Retry import from slskd"
			>
				<FileDown class="h-3.5 w-3.5" />
				{reimport.isPending ? 'Checking...' : 'Retry import'}
			</button>
		</div>
	{/if}
</article>

<style>
	.rung {
		font-size: 10px;
		font-weight: 600;
		font-variant-numeric: tabular-nums;
		padding: 1px 6px;
		border-radius: 9999px;
		color: oklch(from var(--color-base-content) l c h / 0.4);
		background: oklch(from var(--color-base-content) l c h / 0.06);
	}
	.rung-done {
		color: oklch(from var(--color-base-content) l c h / 0.55);
		text-decoration: line-through;
		text-decoration-color: oklch(from var(--color-base-content) l c h / 0.3);
	}
	.rung-now {
		color: oklch(from var(--color-warning) l c h);
		background: oklch(from var(--color-warning) l c h / 0.16);
		box-shadow: inset 0 0 0 1px oklch(from var(--color-warning) l c h / 0.35);
	}
</style>
