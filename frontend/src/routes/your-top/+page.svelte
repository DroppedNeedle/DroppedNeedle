<script lang="ts">
	import TimeRangeView from '$lib/features/discover/components/TimeRangeView.svelte';
	import HomeSection from '$lib/features/discover/components/HomeSection.svelte';
	import { getYourLastfmQuery } from '$lib/features/discover/queries/charts.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { type MusicSource, isMusicSource } from '$lib/stores/musicSource';
	import { Disc3 } from 'lucide-svelte';
	import { PersistedState } from 'runed';
	import { PAGE_SOURCE_KEYS } from '$lib/constants';
	import type { PageProps } from './$types';
	import SimpleSourceSwitcher from '$lib/features/discover/components/SimpleSourceSwitcher.svelte';

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

	// the person's own Last.fm lists, shown under the chart while they have
	// Last.fm linked; nobody else gets any
	const lastfmQuery = getYourLastfmQuery();
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
	<QueryBoundary
		query={lastfmQuery}
		what="your Last.fm lists"
		isEmpty={(lists) => lists.shelves.length === 0}
	>
		{#snippet loading()}{/snippet}
		{#snippet empty()}{/snippet}
		{#snippet children(lists)}
			<div class="space-y-6 pt-4">
				{#each lists.shelves as shelf (shelf.title)}
					<HomeSection section={shelf} madeFrom="home_shelf" />
				{/each}
			</div>
		{/snippet}
	</QueryBoundary>
</div>
