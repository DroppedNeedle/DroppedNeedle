<script lang="ts">
	import { onMount } from 'svelte';
	import AlbumImage from './AlbumImage.svelte';
	import { RotateCcw } from 'lucide-svelte';
	import type { WantedRetryingItem } from '$lib/queries/wanted/types';
	import { nowSeconds, startSharedClock } from '$lib/stores/clock.svelte';

	interface Props {
		item: WantedRetryingItem;
		ownerName?: string;
	}

	let { item, ownerName }: Props = $props();

	onMount(() => startSharedClock());

	function formatEta(seconds: number): string {
		if (seconds <= 60) return 'any moment now';
		if (seconds < 3600) return `in ${Math.round(seconds / 60)} min`;
		if (seconds < 86400) return `in ${Math.round(seconds / 3600)} h`;
		return `in ${Math.round(seconds / 86400)} d`;
	}

	const nextTryLabel = $derived(formatEta(item.next_retry_at - nowSeconds()));
	const attempt = $derived(Math.min(item.retry_count + 1, item.max_attempts));
</script>

<!-- Read-only: this album is still the retry ladder's job (managed from the
     downloads queue); the watcher takes over here once the ladder exhausts. -->
<div class="flex items-center gap-3 sm:gap-4 p-3 sm:p-4 bg-base-200 rounded-box">
	<div class="w-14 h-14 sm:w-16 sm:h-16 shrink-0 rounded-lg overflow-hidden bg-base-300">
		<AlbumImage
			mbid={item.release_group_mbid}
			customUrl={item.cover_url}
			alt={item.album_title}
			size="sm"
			rounded="lg"
			className="w-full h-full"
		/>
	</div>

	<div class="flex-1 min-w-0">
		<a
			href="/album/{item.release_group_mbid}"
			class="block font-semibold text-sm truncate hover:text-accent hover:underline"
			title={item.album_title}
		>
			{item.album_title}
		</a>
		<p class="text-base-content/60 text-xs truncate">
			{item.artist_name}{item.year ? ` • ${item.year}` : ''}
		</p>
		<div class="flex items-center gap-1.5 flex-wrap text-xs mt-0.5">
			<span class="text-warning/80 flex items-center gap-1">
				<RotateCcw class="h-3 w-3" />
				Still hunting
			</span>
			<span class="text-base-content/20">•</span>
			<span class="text-base-content/40">retry {attempt} of {item.max_attempts}</span>
			<span class="text-base-content/20">•</span>
			<span class="text-base-content/40">next try {nextTryLabel}</span>
			{#if ownerName}
				<span class="text-base-content/20">•</span>
				<span class="text-base-content/40">requested by {ownerName}</span>
			{/if}
		</div>
	</div>

	<div class="shrink-0">
		<a href="/downloads" class="btn btn-ghost btn-sm text-base-content/50">Manage in Downloads</a>
	</div>
</div>
