<script lang="ts">
	import type { AdvancedSettingsForm } from './advanced-settings-types';
	import SettingsNumberField from './SettingsNumberField.svelte';

	let { data = $bindable() }: { data: AdvancedSettingsForm } = $props();
</script>

<h4 class="font-medium text-sm text-base-content/70 mb-3">Network & HTTP</h4>
<div class="grid grid-cols-1 sm:grid-cols-3 gap-x-6 gap-y-4 pt-2">
	<SettingsNumberField
		label="Request Timeout"
		description="Default: 10s"
		bind:value={data.http_timeout}
		min={5}
		max={60}
		unit="sec"
	/>
	<SettingsNumberField
		label="Connect Timeout"
		description="Default: 5s"
		bind:value={data.http_connect_timeout}
		min={1}
		max={30}
		unit="sec"
	/>
	<SettingsNumberField
		label="Max Connections"
		description="Pool size (default: 200)"
		bind:value={data.http_max_connections}
		min={50}
		max={500}
		step={10}
	/>
	<SettingsNumberField
		label="Subsonic Search Page Limit"
		description="Per artist, album, or song result page (default: 1000)"
		bind:value={data.subsonic_search_page_limit}
		min={1}
		max={2147483647}
	/>
</div>
<div class="divider my-4"></div>
<h4 class="font-medium text-sm text-base-content/70 mb-3">Batch Processing</h4>
<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4">
	<SettingsNumberField
		label="Artist Image Concurrency"
		description="Parallel image fetches (default: 5)"
		bind:value={data.batch_artist_images}
		min={1}
		max={20}
	/>
	<SettingsNumberField
		label="Album Concurrency"
		description="Parallel album fetches (default: 3)"
		bind:value={data.batch_albums}
		min={1}
		max={20}
	/>
	<SettingsNumberField
		label="Artist Batch Delay"
		description="Default: 0.5s"
		bind:value={data.delay_artist}
		min={0}
		max={5}
		step={0.1}
		unit="sec"
	/>
	<SettingsNumberField
		label="Album Batch Delay"
		description="Default: 1.0s"
		bind:value={data.delay_albums}
		min={0}
		max={5}
		step={0.1}
		unit="sec"
	/>
	<SettingsNumberField
		label="Discovery Warm Interval"
		description="Periodic background refresh (default: 240 min)"
		bind:value={data.artist_discovery_warm_interval}
		min={5}
		max={10080}
		unit="min"
	/>
	<SettingsNumberField
		label="Discovery Warm Delay"
		description="Delay between artists in periodic warm (default: 0.5s)"
		bind:value={data.artist_discovery_warm_delay}
		min={0}
		max={5}
		step={0.1}
		unit="sec"
	/>
	<SettingsNumberField
		label="Discovery Precache Delay"
		description="Delay between artists during library sync precache (default: 0.3s)"
		bind:value={data.artist_discovery_precache_delay}
		min={0}
		max={5}
		step={0.1}
		unit="sec"
	/>
</div>
<div class="divider my-4"></div>
<h4 class="font-medium text-sm text-base-content/70 mb-3">Library Sync</h4>
<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4">
	<SettingsNumberField
		label="Stall Timeout"
		description="Cancel sync if no progress (default: 10 min)"
		bind:value={data.sync_stall_timeout_minutes}
		min={2}
		max={30}
		unit="min"
	/>
	<SettingsNumberField
		label="Max Timeout"
		description="Hard sync timeout (default: 8 hrs)"
		bind:value={data.sync_max_timeout_hours}
		min={1}
		max={48}
		unit="hrs"
	/>
	<SettingsNumberField
		label="AudioDB Prewarm Concurrency"
		description="Parallel AudioDB requests (default: 4)"
		bind:value={data.audiodb_prewarm_concurrency}
		min={1}
		max={8}
	/>
	<SettingsNumberField
		label="AudioDB Prewarm Delay"
		description="Delay between items (default: 0.3s)"
		bind:value={data.audiodb_prewarm_delay}
		min={0}
		max={5}
		step={0.1}
		unit="sec"
	/>
	<SettingsNumberField
		label="Discovery Precache Concurrency"
		description="Parallel discovery fetches (default: 3)"
		bind:value={data.artist_discovery_precache_concurrency}
		min={1}
		max={8}
	/>
</div>
