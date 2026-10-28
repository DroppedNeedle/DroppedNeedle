<script lang="ts">
	import { Play, Shuffle } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import { fromStore } from 'svelte/store';
	import { launchRadio, type RadioMode } from '$lib/player/launchRadio';
	import { deckSampler, type SampleEntry } from '$lib/stores/deckSampler.svelte';
	import { integrationStore } from '$lib/stores/integration';
	import { playbackToast } from '$lib/stores/playbackToast.svelte';
	import type { RadioPlanRequest } from '$lib/queries/discover/types';

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
</script>

<div class="flex items-center gap-2 {className}">
	<Button
		{variant}
		{size}
		icon={Play}
		loading={tuning}
		disabled={shuffling}
		class="[&_svg]:fill-current"
		title="Play a station seeded from this"
		onclick={() => start(false)}
	>
		{tuning ? 'Tuning…' : label}
	</Button>
	{#if showShuffle}
		<Button
			variant="outline"
			{size}
			icon={Shuffle}
			loading={shuffling}
			disabled={tuning}
			title="Shuffle a station seeded from this"
			onclick={() => start(true)}
		>
			Shuffle
		</Button>
	{/if}
</div>
