<script lang="ts">
	import type { AdvancedSettings } from './advanced-settings-types';
	import NumberField from '$lib/ui/NumberField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	let { data = $bindable() }: { data: AdvancedSettings } = $props();
</script>

<div class="alert alert-info alert-soft mb-4">
	<span class="text-sm"
		>Controls how the Discover Queue is built in the background so it's ready when you open
		Discover.</span
	>
</div>
<h4 class="font-medium text-sm text-base-content/70 mb-3">Background Generation</h4>
<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4 pt-2">
	<NumberField
		label="Queue Size"
		help="Albums per queue (default: 10)"
		bind:value={data.discover_queue_size}
		min={1}
		max={20}
		unit="albums"
	/>
	<ToggleRow
		label="Auto-Generate on Visit"
		description="Start building when you visit Discover"
		bind:checked={data.discover_queue_auto_generate}
	/>
	<ToggleRow
		label="Pre-Build in Warm Cycle"
		description="Build queue during periodic cache warming"
		bind:checked={data.discover_queue_warm_cycle_build}
	/>
</div>
<div class="divider my-4"></div>
<h4 class="font-medium text-sm text-base-content/70 mb-3">Queue Tuning</h4>
<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4">
	<NumberField
		label="Seed Artists"
		help="Artists to base recommendations on (default: 3)"
		bind:value={data.discover_queue_seed_artists}
		min={1}
		max={10}
	/>
	<NumberField
		label="Wildcard Slots"
		help="Random variety albums (default: 2)"
		bind:value={data.discover_queue_wildcard_slots}
		min={0}
		max={10}
	/>
	<NumberField
		label="Similar Artists Limit"
		help="Max similar artists per seed (default: 15)"
		bind:value={data.discover_queue_similar_artists_limit}
		min={5}
		max={50}
	/>
	<NumberField
		label="Albums Per Similar Artist"
		help="Top albums fetched per artist (default: 5)"
		bind:value={data.discover_queue_albums_per_similar}
		min={1}
		max={20}
	/>
	<NumberField
		label="Last.fm MBID Lookups"
		help="Max MusicBrainz lookups when matching Last.fm albums (default: 10)"
		bind:value={data.discover_queue_lastfm_mbid_max_lookups}
		min={1}
		max={50}
	/>
</div>
<div class="divider my-4"></div>
<h4 class="font-medium text-sm text-base-content/70 mb-3">Discover Picks</h4>
<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4">
	<NumberField
		label="Discover Picks Count"
		help="How many albums appear in Discover Picks (default: 12)"
		bind:value={data.discover_picks_count}
		min={4}
		max={30}
		step={1}
	/>
	<NumberField
		label="Genre Affinity Weight"
		help="How strongly picks are biased towards your genres (0.0 = random, 1.0 = genre-only, default: 0.7)"
		bind:value={data.discover_picks_genre_affinity_weight}
		min={0}
		max={1}
		step={0.1}
	/>
</div>
