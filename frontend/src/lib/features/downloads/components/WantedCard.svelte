<!--
	A still-hunting album: the app retries it on a ladder of waits. The card
	shows where on the ladder it is, how the last try went, the countdown to
	the next, and Retry now and Stop.
-->
<script lang="ts">
	import { FileDown, RefreshCw, RotateCcw, Square } from 'lucide-svelte';
	import { prefersReducedMotion } from 'svelte/motion';

	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import {
		createReimportDownloadMutation,
		createRetryDownloadMutation,
		createStopAutoRetryMutation
	} from '$lib/features/downloads/queries/DownloadMutations.svelte';
	import {
		canReimport,
		formatCountdown,
		retryLadderState
	} from '$lib/features/downloads/queries/downloadStatus';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { nowSeconds, startSharedClock } from '$lib/stores/clock.svelte';
	import type { DownloadTask } from '$lib/features/downloads/queries/types';
	import { albumHref, artistHref } from '$lib/utils/entityRoutes';
	import { withBasePath } from '$lib/utils/basePath';
	import { formatRelativeTime } from '$lib/utils/formatting';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';

	import CueCountdown from './CueCountdown.svelte';

	let { task }: { task: DownloadTask } = $props();

	const retry = createRetryDownloadMutation();
	const stopRetry = createStopAutoRetryMutation();
	const reimport = createReimportDownloadMutation();

	// this card owns its clock dependency: the shared tick runs while any Wanted card is
	// mounted (refcounted -> still one interval), so the countdown ticks without relying on
	// a parent to start it
	$effect(() => startSharedClock());

	// recomputes every tick because retryLadderState reads the shared clock
	const ladder = $derived(retryLadderState(task, nowSeconds()));
	// Under reduced motion the time left is whole minutes, so it does not tick each second.
	const countdown = $derived.by(() => {
		if (!ladder) return '';
		const left = ladder.secondsUntilNext;
		if (prefersReducedMotion.current && left < 3600) return `${Math.max(1, Math.ceil(left / 60))}m`;
		return formatCountdown(left);
	});
	const urgent = $derived(!!ladder && ladder.secondsUntilNext <= 60);
	const lastTried = $derived(
		task.completed_at ? formatRelativeTime(new Date(task.completed_at * 1000)) : null
	);
	const isPartial = $derived(task.status === 'partial');
	const facts = $derived(
		[
			isPartial && task.files_total > 0
				? `Got ${task.files_completed}/${task.files_total} tracks`
				: null,
			lastTried ? `Last tried ${lastTried}` : null
		].filter(Boolean)
	);

	function rungLabel(minutes: number): string {
		return minutes < 60 ? `${minutes}m` : `${Math.round(minutes / 60)}h`;
	}

	// A library album whose guess is this album, though its files name
	// another: curators open its edition sheet, everyone else its page.
	function maybeHaveHref(albumId: string): string {
		const page = `/album/${encodeURIComponent(albumId)}`;
		return withBasePath(authStore.isTrusted ? `${page}?edition=choose&from=downloads` : page);
	}

	let retried = $state(false);
	function onRetryNow() {
		retried = true;
		retry.mutate(task.id, { onError: () => (retried = false) });
	}
</script>

<Card as="article" variant="bordered" padding="none" class="border-warning!">
	<div class="hunt">
		<div class="hunt__cover">
			<AlbumImage
				mbid={task.release_group_mbid}
				alt={task.album_title}
				size="full"
				requestSize={250}
				rounded="none"
				className="h-full w-full"
			/>
		</div>

		<div class="hunt__who">
			<h3 class="hunt__title">
				<a href={albumHref(task.release_group_mbid)} class="hunt__link">{task.album_title}</a>
			</h3>
			<p class="hunt__artist">
				{#if task.artist_mbid}
					<a href={artistHref(task.artist_mbid)} class="hunt__artist-link">{task.artist_name}</a>
				{:else}
					{task.artist_name}
				{/if}
				{#if task.year}<span class="text-muted mr-1.5 ml-0.5">·</span>{task.year}{/if}
				{#if task.download_type === 'track' && task.track_title}<span
						class="text-muted mr-1.5 ml-0.5">·</span
					>{task.track_title}{/if}
			</p>
		</div>

		<div class="hunt__state">
			<StatusChip tone="warning" icon={RefreshCw}>
				{#if ladder}Next spin · retry {ladder.attempt} of {ladder.total}{:else}Retrying{/if}
			</StatusChip>

			{#if ladder}
				<ol class="hunt__ladder" aria-label="Retry schedule">
					{#each ladder.rungs as min, i (i)}
						<li
							class="hunt__rung"
							class:is-done={i < ladder.index}
							class:is-now={i === ladder.index}
							aria-current={i === ladder.index ? 'step' : undefined}
						>
							<span class="hunt__step">{rungLabel(min)}</span>
						</li>
					{/each}
				</ol>
			{/if}

			{#if facts.length > 0}
				<p class="hunt__facts">{facts.join(' · ')}</p>
			{/if}
			{#if task.reason}
				<p class="hunt__reason line-clamp-2">Last try: {task.reason.text}</p>
			{/if}
			{#if task.maybe_have}
				<p class="hunt__maybe">
					You may have this: {task.maybe_have.title} is a guess.
					<a class="link" href={maybeHaveHref(task.maybe_have.album_id)}>Check it</a>
				</p>
			{/if}
		</div>

		<div class="hunt__clock">
			<CueCountdown fraction={ladder?.fractionElapsed ?? 0} label={countdown} {urgent} />
		</div>

		<div class="hunt__actions">
			<Button
				variant="primary"
				size="display-sm"
				icon={RotateCcw}
				onclick={onRetryNow}
				disabled={retry.isPending || retried}
				title="Try this download again right now"
			>
				{retried ? 'Retrying…' : 'Retry now'}
			</Button>
			<Button
				variant="outline"
				size="display-sm"
				icon={Square}
				onclick={() => stopRetry.mutate(task.id)}
				disabled={stopRetry.isPending}
				title="Stop hunting for this - it won't be watched for later either"
			>
				Stop
			</Button>
			{#if authStore.isAdmin && canReimport(task)}
				<Button
					variant="ghost"
					size="sm"
					icon={FileDown}
					onclick={() =>
						reimport.mutate({ id: task.id, release_group_mbid: task.release_group_mbid })}
					disabled={reimport.isPending}
					title="Already fixed this in the download client? Check the downloads mount again without re-searching."
					aria-label="Retry import"
				>
					{reimport.isPending ? 'Checking...' : 'Retry import'}
				</Button>
			{/if}
		</div>
	</div>
</Card>

<style>
	.hunt {
		display: grid;
		grid-template-columns: 8rem minmax(9rem, 14rem) minmax(0, 1fr) auto 11rem;
		align-items: center;
		gap: 1rem 1.75rem;
		padding: 0.875rem 2rem 0.875rem 1.75rem;
	}

	.hunt__cover {
		width: 8rem;
		height: 8rem;
		overflow: hidden;
		border-radius: 0.25rem;
	}

	.hunt__who {
		align-self: start;
		min-width: 0;
		padding-top: 0.5rem;
	}
	.hunt__title {
		font-size: 1.375rem;
		font-weight: 700;
		line-height: 1.2;
		overflow-wrap: anywhere;
	}
	.hunt__link {
		text-decoration: underline;
		text-decoration-thickness: 2px;
		text-underline-offset: 4px;
		border-radius: 0.25rem;
	}
	.hunt__link:hover,
	.hunt__artist-link:hover {
		color: var(--dn-title);
	}
	.hunt__artist {
		margin-top: 0.5rem;
		font-size: 1rem;
	}

	.hunt__state {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.625rem;
		min-width: 0;
	}

	.hunt__ladder {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.25rem 0;
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 600;
	}
	.hunt__rung {
		display: inline-flex;
		align-items: center;
		color: var(--dn-muted);
	}
	/* the line between two steps */
	.hunt__rung + .hunt__rung::before {
		content: '';
		width: 1.5rem;
		height: 1px;
		margin-inline: 0.125rem;
		background: var(--dn-edge);
	}
	.hunt__step {
		padding: 0.125rem 0.625rem;
		border-radius: 9999px;
	}
	.hunt__rung.is-done .hunt__step {
		color: var(--color-base-content);
		text-decoration: line-through;
	}
	.hunt__rung.is-now .hunt__step {
		background: var(--color-warning);
		color: var(--color-warning-content);
	}

	.hunt__facts {
		font-family: var(--font-mono);
		font-size: 0.875rem;
		font-weight: 500;
		letter-spacing: 0.1em;
		text-transform: uppercase;
	}
	.hunt__reason {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		color: var(--dn-muted);
	}
	.hunt__maybe {
		font-size: 0.75rem;
		color: var(--dn-muted);
	}

	.hunt__actions {
		display: flex;
		flex-direction: column;
		align-items: stretch;
		gap: 0.75rem;
	}

	@media (max-width: 1279px) {
		.hunt {
			grid-template-columns: 7rem minmax(0, 1fr) auto;
			padding: 1rem 1.25rem;
		}
		.hunt__cover {
			width: 7rem;
			height: 7rem;
			grid-row: span 2;
		}
		.hunt__who {
			grid-column: 2 / -1;
			padding-top: 0;
		}
		.hunt__state {
			grid-column: 2;
		}
		.hunt__clock {
			grid-column: 3;
		}
		.hunt__actions {
			grid-column: 1 / -1;
			flex-direction: row;
			flex-wrap: wrap;
			justify-content: flex-end;
		}
	}

	@media (max-width: 639px) {
		.hunt {
			grid-template-columns: 5.5rem minmax(0, 1fr);
			gap: 0.875rem 1rem;
			padding: 1rem;
		}
		.hunt__cover {
			width: 5.5rem;
			height: 5.5rem;
			grid-row: auto;
		}
		.hunt__who {
			grid-column: 2;
			align-self: center;
		}
		.hunt__title {
			font-size: 1.125rem;
		}
		.hunt__artist {
			margin-top: 0.25rem;
			font-size: 0.875rem;
		}
		.hunt__state {
			grid-column: 1 / -1;
		}
		.hunt__facts {
			font-size: 0.75rem;
			letter-spacing: 0.06em;
		}
		.hunt__clock {
			grid-column: 1 / -1;
			justify-self: center;
		}
		.hunt__actions {
			grid-column: 1 / -1;
			display: grid;
			grid-template-columns: 1fr 1fr;
		}
	}
</style>
