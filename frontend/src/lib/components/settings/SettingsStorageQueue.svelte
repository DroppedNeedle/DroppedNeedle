<script lang="ts">
	import type { AdvancedSettingsForm } from './advanced-settings-types';
	import SettingsNumberField from './SettingsNumberField.svelte';

	let { data = $bindable() }: { data: AdvancedSettingsForm } = $props();
</script>

<h4 class="font-medium text-sm text-base-content/70 mb-3">Memory and Disk Storage</h4>
<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4 pt-2">
	<SettingsNumberField
		label="Memory Cache Max Entries"
		description="Maximum items kept in memory (default: 10,000)"
		bind:value={data.memory_cache_max_entries}
		min={1000}
		max={100000}
		step={1000}
	/>
	<SettingsNumberField
		label="Memory Cleanup Interval"
		description="How often old in-memory items are cleaned up (default: 5 min)"
		bind:value={data.memory_cache_cleanup_interval}
		min={60}
		max={3600}
		step={60}
		unit="sec"
	/>
	<SettingsNumberField
		label="Cover Memory Cache Max Entries"
		description="Frequently used cover images kept in memory (default: 128)"
		bind:value={data.cover_memory_cache_max_entries}
		min={16}
		max={2048}
		step={16}
	/>
	<SettingsNumberField
		label="Cover Memory Cache Size"
		description="Maximum space for frequently used cover images (default: 16 MB)"
		bind:value={data.cover_memory_cache_max_size_mb}
		min={1}
		max={1024}
		unit="MB"
	/>
	<SettingsNumberField
		label="Disk Cleanup Interval"
		description="How often old cache files are cleaned up (default: 10 min)"
		bind:value={data.disk_cache_cleanup_interval}
		min={1}
		max={60}
		unit="min"
	/>
	<SettingsNumberField
		label="Metadata Cache Limit"
		description="Disk space reserved for cached metadata (default: 500 MB)"
		bind:value={data.recent_metadata_max_size_mb}
		min={100}
		max={5000}
		unit="MB"
	/>
	<SettingsNumberField
		label="Cover Cache Limit"
		description="Disk space reserved for cover art (default: 1 GB)"
		bind:value={data.recent_covers_max_size_mb}
		min={100}
		max={10000}
		unit="MB"
	/>
</div>
<div class="divider my-4"></div>
<h4 class="font-medium text-sm text-base-content/70 mb-3">Discover Queue</h4>
<div class="alert alert-info alert-soft mb-4">
	<span class="text-sm"
		>Controls how the Discover Queue is built in the background so it's ready when you open
		Discover.</span
	>
</div>
<h4 class="font-medium text-sm text-base-content/70 mb-3">Background Generation</h4>
<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4 pt-2">
	<SettingsNumberField
		label="Queue Size"
		description="Albums per queue (default: 10)"
		bind:value={data.discover_queue_size}
		min={1}
		max={20}
		unit="albums"
	/>
	<SettingsNumberField
		label="Queue Freshness"
		description="Build a fresh queue after this much time (default: 24h)"
		bind:value={data.discover_queue_ttl}
		min={1}
		max={168}
		unit="hours"
	/>
	<fieldset class="fieldset">
		<legend class="fieldset-legend">Auto-Generate on Visit</legend>
		<label class="label cursor-pointer gap-3">
			<span class="text-sm">Start building when you visit Discover</span>
			<input
				type="checkbox"
				bind:checked={data.discover_queue_auto_generate}
				class="toggle toggle-primary"
			/>
		</label>
	</fieldset>
	<fieldset class="fieldset">
		<legend class="fieldset-legend">Pre-Build in Warm Cycle</legend>
		<label class="label cursor-pointer gap-3">
			<span class="text-sm">Build queue during periodic cache warming</span>
			<input
				type="checkbox"
				bind:checked={data.discover_queue_warm_cycle_build}
				class="toggle toggle-primary"
			/>
		</label>
	</fieldset>
	<SettingsNumberField
		label="Status Polling Interval"
		description="How often the app checks queue progress (default: 4s)"
		bind:value={data.discover_queue_polling_interval}
		min={1}
		max={30}
		unit="sec"
	/>
</div>
<div class="divider my-4"></div>
<h4 class="font-medium text-sm text-base-content/70 mb-3">Queue Tuning</h4>
<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4">
	<SettingsNumberField
		label="Seed Artists"
		description="Artists to base recommendations on (default: 3)"
		bind:value={data.discover_queue_seed_artists}
		min={1}
		max={10}
	/>
	<SettingsNumberField
		label="Wildcard Slots"
		description="Random variety albums (default: 2)"
		bind:value={data.discover_queue_wildcard_slots}
		min={0}
		max={10}
	/>
	<SettingsNumberField
		label="Similar Artists Limit"
		description="Max similar artists per seed (default: 15)"
		bind:value={data.discover_queue_similar_artists_limit}
		min={5}
		max={50}
	/>
	<SettingsNumberField
		label="Albums Per Similar Artist"
		description="Top albums fetched per artist (default: 5)"
		bind:value={data.discover_queue_albums_per_similar}
		min={1}
		max={20}
	/>
	<SettingsNumberField
		label="Card Details Cache"
		description="How long album details on queue cards stay fresh (default: 24h)"
		bind:value={data.discover_queue_enrich_ttl}
		min={1}
		max={168}
		unit="hours"
	/>
	<SettingsNumberField
		label="Last.fm MBID Lookups"
		description="Max MusicBrainz lookups when matching Last.fm albums (default: 10)"
		bind:value={data.discover_queue_lastfm_mbid_max_lookups}
		min={1}
		max={50}
	/>
</div>
<div class="divider my-4"></div>
<h4 class="font-medium text-sm text-base-content/70 mb-3">Discover Picks</h4>
<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4">
	<SettingsNumberField
		label="Discover Picks Count"
		description="How many albums appear in Discover Picks (default: 12)"
		bind:value={data.discover_picks_count}
		min={4}
		max={30}
		step={1}
	/>
	<SettingsNumberField
		label="Genre Affinity Weight"
		description="How strongly picks are biased towards your genres (0.0 = random, 1.0 = genre-only, default: 0.7)"
		bind:value={data.discover_picks_genre_affinity_weight}
		min={0}
		max={1}
		step={0.1}
	/>
</div>
