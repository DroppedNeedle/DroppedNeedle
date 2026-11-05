<script lang="ts">
	import { onMount } from 'svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import { RotateCcw } from 'lucide-svelte';
	import type { WantedRetryingItem } from '$lib/features/requests/queries/wanted.svelte';
	import { nowSeconds, startSharedClock } from '$lib/stores/clock.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import RequestRow from './RequestRow.svelte';

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

	const nextTryLabel = $derived(
		item.next_retry_at == null ? null : formatEta(item.next_retry_at - nowSeconds())
	);
	const attempt = $derived(Math.min(item.retry_count + 1, item.max_attempts));
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
	<Button variant="outline" size="display-sm" href={withBasePath('/downloads')}>
		Manage in Downloads
	</Button>
{/snippet}

<!-- Read-only: this album is still the retry ladder's job (managed from the
     downloads queue); the watcher takes over here once the ladder exhausts. -->
<RequestRow {cover} {aside}>
	<div class="rq-head">
		<a
			href={withBasePath(`/album/${item.musicbrainz_id}`)}
			class="rq-title"
			title={item.album_title}
		>
			{item.album_title}
		</a>
		<span class="rq-pill rq-pill--warning">
			<RotateCcw aria-hidden="true" />
			Still hunting
		</span>
	</div>
	<p class="rq-artist">
		{item.artist_name}{item.year ? ` • ${item.year}` : ''}
	</p>
	<p class="rq-meta mt-2">
		<span>retry {attempt} of {item.max_attempts}</span>
		{#if nextTryLabel}
			<span>next try {nextTryLabel}</span>
		{/if}
		{#if ownerName}
			<span>requested by {ownerName}</span>
		{/if}
	</p>
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
</style>
