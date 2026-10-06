<script lang="ts">
	import TimeRangeView from '$lib/components/TimeRangeView.svelte';
	import { type MusicSource, isMusicSource } from '$lib/stores/musicSource';
	import { Disc3 } from 'lucide-svelte';
	import { PersistedState } from 'runed';
	import { PAGE_SOURCE_KEYS } from '$lib/constants';
	import type { PageProps } from './$types';
	import SimpleSourceSwitcher from '$lib/components/SimpleSourceSwitcher.svelte';

	const { data }: PageProps = $props();

	// svelte-ignore state_referenced_locally
	let activeSource = new PersistedState<MusicSource>(
		PAGE_SOURCE_KEYS['yourTop'],
		data.primarySource
	);

	let validSource = $derived(
		isMusicSource(activeSource.current) ? activeSource.current : data.primarySource
	);

	function handleSourceChange(nextSource: MusicSource) {
		activeSource.current = nextSource;
	}

	let sourceLabel = $derived(validSource === 'lastfm' ? 'Last.fm' : 'ListenBrainz');
</script>

<svelte:head>
	<title>Your Top Albums - DroppedNeedle</title>
</svelte:head>

<div class="space-y-4 px-4 sm:px-6 lg:px-8">
	<div class="flex justify-end">
		<SimpleSourceSwitcher currentSource={validSource} onSourceChange={handleSourceChange} />
	</div>
	<TimeRangeView
		itemType="album"
		chart="your-top-albums"
		title="Your Top Albums"
		subtitle={`Your most listened albums on ${sourceLabel}`}
		source={validSource}
		errorIcon={Disc3}
	/>
</div>
