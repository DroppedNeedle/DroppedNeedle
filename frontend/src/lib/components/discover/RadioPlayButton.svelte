<script lang="ts">
	import { LoaderCircle, Play, Shuffle } from 'lucide-svelte';
	import { fromStore } from 'svelte/store';
	import { launchRadio, type RadioMode } from '$lib/player/launchRadio';
	import { deckSampler, type SampleEntry } from '$lib/stores/deckSampler.svelte';
	import { integrationStore } from '$lib/stores/integration';
	import { playbackToast } from '$lib/stores/playbackToast.svelte';
	import type { RadioPlanRequest } from '$lib/types';

	interface Props {
		seed: Omit<RadioPlanRequest, 'exclude_recording_mbids' | 'fast'>;
		mode?: RadioMode;
		forcePreviews?: boolean;
		/** Preview-mode station (album entries). Previews play in the floating widget,
		 * never the main player (cross-origin clips are muted by the player's Web Audio). */
		previewStation?: { title: string; entries: SampleEntry[] } | null;
		showShuffle?: boolean;
		size?: 'xs' | 'sm' | 'md';
		variant?: 'primary' | 'ghost';
		label?: string;
		className?: string;
	}

	let {
		seed,
		mode,
		forcePreviews = false,
		previewStation = null,
		showShuffle = false,
		size = 'sm',
		variant = 'primary',
		label = 'Play',
		className = ''
	}: Props = $props();

	const integrations = fromStore(integrationStore);
	const ytConfigured = $derived(!!integrations.current.youtube_api);

	let tuning = $state(false);
	let shuffling = $state(false);

	function shuffled<T>(items: T[]): T[] {
		const out = [...items];
		for (let i = out.length - 1; i > 0; i--) {
			const j = Math.floor(Math.random() * (i + 1));
			[out[i], out[j]] = [out[j], out[i]];
		}
		return out;
	}

	async function start(shuffle: boolean) {
		if (tuning || shuffling) return;
		// preview mode plays a 30s-clip station in the floating widget (never the
		// main player - cross-origin clips are muted there); it must NOT silently
		// fall through to a full-track station when there are no albums to sample
		if (forcePreviews && previewStation) {
			if (previewStation.entries.length === 0) {
				playbackToast.show('No previews available for this yet', 'warning');
				return;
			}
			const entries = shuffle ? shuffled(previewStation.entries) : previewStation.entries;
			deckSampler.startStation(previewStation.title, entries);
			return;
		}
		if (shuffle) {
			shuffling = true;
		} else {
			tuning = true;
		}
		try {
			await launchRadio(seed, ytConfigured, { shuffle, mode });
		} finally {
			tuning = false;
			shuffling = false;
		}
	}

	const sizeClass = $derived(size === 'xs' ? 'btn-xs' : size === 'md' ? 'btn-md' : 'btn-sm');
	const variantClass = $derived(variant === 'ghost' ? 'btn-ghost' : 'btn-primary');
</script>

<div class="flex items-center gap-2 {className}">
	<button
		class="btn {sizeClass} {variantClass} gap-2"
		onclick={() => start(false)}
		disabled={tuning || shuffling}
		title="Play a station seeded from this"
	>
		{#if tuning}
			<LoaderCircle class="h-4 w-4 animate-spin" />
			Tuning…
		{:else}
			<Play class="h-4 w-4" fill="currentColor" />
			{label}
		{/if}
	</button>
	{#if showShuffle}
		<button
			class="btn {sizeClass} btn-outline gap-2"
			onclick={() => start(true)}
			disabled={tuning || shuffling}
			title="Shuffle a station seeded from this"
		>
			{#if shuffling}
				<LoaderCircle class="h-4 w-4 animate-spin" />
			{:else}
				<Shuffle class="h-4 w-4" />
			{/if}
			Shuffle
		</button>
	{/if}
</div>
