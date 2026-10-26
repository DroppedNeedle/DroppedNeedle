<script lang="ts">
	import { onMount } from 'svelte';
	import AlbumImage from './AlbumImage.svelte';
	import { CircleCheck, CircleStop, Play, Radar, RefreshCw, Sparkles, X } from 'lucide-svelte';
	import type { WantedWatchItem } from '$lib/queries/wanted/types';
	import { nowSeconds, startSharedClock } from '$lib/stores/clock.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	interface Props {
		item: WantedWatchItem;
		busy?: boolean;
		ownerName?: string;
		onstop?: (item: WantedWatchItem) => void;
		onresume?: (item: WantedWatchItem) => void;
		onseen?: (item: WantedWatchItem) => void;
	}

	let { item, busy = false, ownerName, onstop, onresume, onseen }: Props = $props();

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

<Card padding="none">
	<div class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4">
		<div class="w-14 h-14 sm:w-16 sm:h-16 shrink-0 rounded-lg overflow-hidden bg-base-300">
			<AlbumImage
				mbid={item.musicbrainz_id}
				customUrl={item.cover_url ?? null}
				alt={item.album_title}
				size="sm"
				rounded="lg"
				className="w-full h-full"
			/>
		</div>

		<div class="flex-1 min-w-0">
			<a
				href={withBasePath(`/album/${item.musicbrainz_id}`)}
				class="block font-semibold text-sm truncate hover:text-accent hover:underline"
				title={item.album_title}
			>
				{item.album_title}
			</a>
			<p class="text-base-content/60 text-xs truncate">
				{item.artist_name}{item.year ? ` • ${item.year}` : ''}
			</p>

			<div class="flex items-center gap-1.5 flex-wrap text-xs mt-0.5">
				{#if item.state === 'watching'}
					<span class="watch-dot shrink-0" aria-hidden="true"></span>
					<span class="text-info/80 flex items-center gap-1">
						<Radar class="h-3 w-3" />
						Watching
					</span>
					{#if nextCheckLabel}
						<span class="text-base-content/20">•</span>
						<span class="text-base-content/40">next check {nextCheckLabel}</span>
					{/if}
					<span class="text-base-content/20">•</span>
					<span class="text-base-content/40">{checkedLabel}</span>
				{:else if item.state === 'stopped'}
					<span class="text-base-content/50 flex items-center gap-1">
						<CircleStop class="h-3 w-3" />
						Stopped
					</span>
					<span class="text-base-content/20">•</span>
					<span class="text-base-content/40">not being searched</span>
				{:else}
					<span class="text-success/80 flex items-center gap-1">
						<CircleCheck class="h-3 w-3" />
						Found
					</span>
					<span class="text-base-content/20">•</span>
					<span class="text-base-content/40">in your library</span>
				{/if}
				{#if item.kind === 'partial' && item.state !== 'fulfilled'}
					<span class="text-base-content/20">•</span>
					<span class="text-base-content/40">finding missing tracks</span>
				{/if}
				{#if ownerName}
					<span class="text-base-content/20">•</span>
					<span class="text-base-content/40">watched for {ownerName}</span>
				{/if}
			</div>

			{#if item.new_candidate_count > 0}
				<a
					href={withBasePath(`/album/${item.musicbrainz_id}`)}
					class="inline-flex items-center gap-1 mt-1.5 px-2 py-0.5 rounded-full bg-accent/15 text-accent text-xs font-medium hover:bg-accent/25"
					onclick={() => onseen?.(item)}
				>
					<Sparkles class="h-3 w-3" />
					{item.new_candidate_count} new {item.new_candidate_count === 1
						? 'candidate'
						: 'candidates'} to review
				</a>
			{/if}
		</div>

		<div class="flex gap-2 shrink-0">
			{#if item.state === 'watching'}
				{#if onresume}
					<Button
						variant="ghost"
						size="sm"
						icon={RefreshCw}
						class="gap-1"
						disabled={busy}
						onclick={() => onresume?.(item)}
						title="Check for this album again soon"
						aria-label="Check now"
					>
						<span class="hidden sm:inline">Check now</span>
					</Button>
				{/if}
				{#if onstop}
					<Button
						variant="danger-ghost"
						size="sm"
						icon={X}
						class="gap-1"
						disabled={busy}
						onclick={() => onstop?.(item)}
						title="Stop hunting for this - it won't be watched"
						aria-label="Stop"
					>
						<span class="hidden sm:inline">Stop</span>
					</Button>
				{/if}
			{:else if item.state === 'stopped' && onresume}
				<Button
					variant="outline"
					size="sm"
					icon={Play}
					class="gap-1"
					disabled={busy}
					onclick={() => onresume?.(item)}
				>
					Resume
				</Button>
			{/if}
		</div>
	</div>
</Card>

<style>
	.watch-dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: oklch(from var(--color-info) l c h / 0.7);
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
