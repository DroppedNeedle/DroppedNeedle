<script lang="ts">
	import { LoaderCircle, Play, Square } from 'lucide-svelte';
	import { deckSampler } from '$lib/stores/deckSampler.svelte';

	interface Props {
		/** stable id, e.g. the release-group mbid or `track:artist|title` */
		sampleKey: string;
		artist: string;
		title: string;
		kind?: 'album' | 'track';
		size?: 'xs' | 'sm' | 'md';
		/** navigation/cover context for the floating widget (optional) */
		albumMbid?: string | null;
		artistMbid?: string | null;
		coverUrl?: string | null;
	}

	let {
		sampleKey,
		artist,
		title,
		kind = 'album',
		size = 'sm',
		albumMbid = null,
		artistMbid = null,
		coverUrl = null
	}: Props = $props();

	const active = $derived(deckSampler.activeKey === sampleKey && deckSampler.status !== 'idle');
	const loading = $derived(active && deckSampler.status === 'loading');
	const RING_R = 8;
	const RING_C = 2 * Math.PI * RING_R;

	function toggle(e: MouseEvent) {
		e.stopPropagation();
		e.preventDefault();
		if (active) {
			deckSampler.stop();
			return;
		}
		const ctx = {
			albumMbid: albumMbid ?? (kind === 'album' ? sampleKey : null),
			artistMbid,
			coverUrl
		};
		if (kind === 'track') {
			deckSampler.startTrack(sampleKey, artist, title, ctx);
		} else {
			deckSampler.start(sampleKey, artist, title, ctx);
		}
	}
</script>

<button
	type="button"
	class="relative {size === 'xs'
		? 'flex h-6 w-6 items-center justify-center rounded-full text-primary hover:bg-base-content/10'
		: `btn btn-circle btn-sm ${size === 'sm' ? 'min-h-[36px] min-w-[36px]' : 'min-h-[44px] min-w-[44px]'} border-none bg-base-content/10 shadow-sm hover:bg-base-content/20`} active:scale-[0.95]"
	title={active
		? 'Stop preview'
		: kind === 'album'
			? 'Preview this album (30s samples)'
			: 'Preview this track (30s)'}
	aria-label="{active ? 'Stop' : 'Play'} preview of {title}"
	onclick={toggle}
>
	{#if loading}
		<LoaderCircle class="h-4 w-4 animate-spin" />
	{:else if active}
		<Square class="h-3.5 w-3.5" fill="currentColor" />
		<svg viewBox="0 0 20 20" class="pointer-events-none absolute inset-0 h-full w-full -rotate-90">
			<circle
				cx="10"
				cy="10"
				r={RING_R}
				fill="none"
				class="stroke-primary"
				stroke-width="1.5"
				stroke-dasharray={RING_C}
				stroke-dashoffset={RING_C * (1 - deckSampler.progress)}
			/>
		</svg>
	{:else}
		<Play class="h-4 w-4" fill="currentColor" />
	{/if}
</button>
