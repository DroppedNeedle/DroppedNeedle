<script lang="ts">
	import { onMount } from 'svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import {
		CircleCheck,
		CircleStop,
		Play,
		Radar,
		RefreshCw,
		Sparkles,
		Trash2,
		X
	} from 'lucide-svelte';
	import type { WantedWatchItem } from '$lib/features/requests/queries/wanted.svelte';
	import { nowSeconds, startSharedClock } from '$lib/stores/clock.svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import RequestRow from './RequestRow.svelte';

	interface Props {
		item: WantedWatchItem;
		busy?: boolean;
		ownerName?: string;
		onstop?: (item: WantedWatchItem) => void;
		onresume?: (item: WantedWatchItem) => void;
		onseen?: (item: WantedWatchItem) => void;
		onremove?: (item: WantedWatchItem) => void;
	}

	let { item, busy = false, ownerName, onstop, onresume, onseen, onremove }: Props = $props();

	onMount(() => startSharedClock());

	function formatEta(seconds: number): string {
		if (seconds <= 60) return 'any moment now';
		if (seconds < 3600) return `in ${Math.round(seconds / 60)} min`;
		if (seconds < 86400) return `in ${Math.round(seconds / 3600)} h`;
		return `in ${Math.round(seconds / 86400)} d`;
	}

	const nextCheckLabel = $derived(
		item.next_check_at == null ? null : formatEta(item.next_check_at - nowSeconds())
	);
	const checkedLabel = $derived(
		item.check_count > 0 ? `checked ${item.check_count}×` : 'not checked yet'
	);
</script>

{#snippet cover()}
	<AlbumImage
		mbid={item.musicbrainz_id}
		customUrl={item.cover_url ?? null}
		alt={item.album_title}
		size="sm"
		rounded="none"
		className="w-full h-full"
	/>
{/snippet}

{#snippet aside()}
	<div class="rq-watch-actions flex flex-wrap items-center justify-end gap-2.5">
		{#if item.state === 'watching'}
			{#if onresume}
				<Button
					variant="outline"
					size="display-sm"
					icon={RefreshCw}
					disabled={busy}
					onclick={() => onresume?.(item)}
					title="Check for this album again soon"
					aria-label="Check now"
				>
					<span class="max-sm:hidden">Check now</span>
				</Button>
			{/if}
			{#if onstop}
				<Button
					variant="outline"
					size="display-sm"
					icon={X}
					disabled={busy}
					onclick={() => onstop?.(item)}
					title="Stop hunting for this - it won't be watched"
					aria-label="Stop"
				>
					<span class="max-sm:hidden">Stop</span>
				</Button>
			{/if}
		{:else if item.state === 'stopped' && onresume}
			<Button
				variant="outline"
				size="display-sm"
				icon={Play}
				disabled={busy}
				onclick={() => onresume?.(item)}
			>
				Resume
			</Button>
		{/if}
		{#if onremove}
			<IconButton
				variant="outline"
				size="display-sm"
				shape="square"
				icon={Trash2}
				label="Remove from the list"
				disabled={busy}
				onclick={() => onremove?.(item)}
			/>
		{/if}
	</div>
{/snippet}

<RequestRow {cover} {aside}>
	<div class="rq-head">
		<a
			href={withBasePath(`/album/${item.musicbrainz_id}`)}
			class="rq-title"
			title={item.album_title}
		>
			{item.album_title}
		</a>
		{#if item.state === 'watching'}
			<span class="rq-pill rq-pill--info">
				<span class="watch-dot" aria-hidden="true"></span>
				<Radar aria-hidden="true" />
				Watching
			</span>
		{:else if item.state === 'stopped'}
			<span class="rq-pill rq-pill--neutral rq-pill--outline">
				<CircleStop aria-hidden="true" />
				Stopped
			</span>
		{:else}
			<span class="rq-pill rq-pill--success rq-pill--outline">
				<CircleCheck aria-hidden="true" />
				Found
			</span>
		{/if}
	</div>
	<p class="rq-artist">
		{item.artist_name}{item.year ? ` • ${item.year}` : ''}
	</p>
	<p class="rq-meta mt-2">
		{#if item.state === 'watching'}
			{#if nextCheckLabel}
				<span>Next check {nextCheckLabel}</span>
			{/if}
			<span>{checkedLabel}</span>
			{#if item.last_outcome === 'outside_settings'}
				<span>only copies your settings refuse</span>
			{/if}
		{:else if item.state === 'stopped'}
			<span>not being searched</span>
		{:else}
			<span>in your library</span>
		{/if}
		{#if item.kind === 'partial' && item.state !== 'fulfilled'}
			<span>finding missing tracks</span>
		{/if}
		{#if ownerName}
			<span>watched for {ownerName}</span>
		{/if}
	</p>

	{#if item.new_candidate_count > 0}
		<a
			href={withBasePath(`/album/${item.musicbrainz_id}`)}
			class="rq-pill rq-pill--action mt-3"
			onclick={() => onseen?.(item)}
		>
			<Sparkles aria-hidden="true" />
			{item.new_candidate_count} new {item.new_candidate_count === 1 ? 'candidate' : 'candidates'} to
			review
		</a>
	{/if}
</RequestRow>

<style>
	.rq-head {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.375rem 1.25rem;
		margin-bottom: 0.25rem;
	}
	.rq-head .rq-title {
		min-width: 0;
	}

	/* on a phone Check now and Stop show their icons only, as squares */
	@media (max-width: 639px) {
		.rq-watch-actions :global(.btn[aria-label]) {
			width: var(--size);
			padding: 0;
		}
	}

	.watch-dot {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background-color: currentColor;
		animation: watch-dot-pulse 2.5s ease-in-out infinite;
	}

	@keyframes watch-dot-pulse {
		0%,
		100% {
			opacity: 0.3;
			transform: scale(0.8);
		}
		50% {
			opacity: 1;
			transform: scale(1.2);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.watch-dot {
			animation: none !important;
			opacity: 0.8;
			transform: none;
		}
	}
</style>
