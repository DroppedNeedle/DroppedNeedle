<script lang="ts">
	import type { AdvancedSettingsForm } from './advanced-settings-types';
	import NumberField from '$lib/ui/NumberField.svelte';

	let { data = $bindable() }: { data: AdvancedSettingsForm } = $props();
</script>

<h4 class="font-medium text-sm text-base-content/70 mb-3">Network & HTTP</h4>
<div class="grid grid-cols-1 sm:grid-cols-3 gap-x-6 gap-y-4 pt-2">
	<NumberField
		label="Request Timeout"
		help="Default: 10s"
		bind:value={data.http_timeout}
		min={5}
		max={60}
		unit="sec"
	/>
	<NumberField
		label="Connect Timeout"
		help="Default: 5s"
		bind:value={data.http_connect_timeout}
		min={1}
		max={30}
		unit="sec"
	/>
	<NumberField
		label="Max Connections"
		help="Pool size (default: 200)"
		bind:value={data.http_max_connections}
		min={50}
		max={500}
		step={10}
	/>
</div>
<div class="divider my-4"></div>
<h4 class="font-medium text-sm text-base-content/70 mb-3">Library Image Caching</h4>
<p class="text-sm text-base-content/60 mb-3">
	How fast a library image refresh works through your artists and albums. Lower numbers are gentler
	on the providers.
</p>
<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4">
	<NumberField
		label="Artist Image Concurrency"
		help="Artists fetched at once (default: 10)"
		bind:value={data.batch_artist_images}
		min={1}
		max={20}
	/>
	<NumberField
		label="Album Concurrency"
		help="Albums fetched at once; adjusts to provider speed (default: 8)"
		bind:value={data.batch_albums}
		min={1}
		max={20}
	/>
	<NumberField
		label="Artist Batch Delay"
		help="Default: 0.5s"
		bind:value={data.delay_artist}
		min={0}
		max={5}
		step={0.1}
		unit="sec"
	/>
	<NumberField
		label="Album Batch Delay"
		help="Default: 0.3s"
		bind:value={data.delay_albums}
		min={0}
		max={5}
		step={0.1}
		unit="sec"
	/>
	<NumberField
		label="Discovery Concurrency"
		help="Artists whose similar artists and top tracks are fetched at once (default: 5)"
		bind:value={data.artist_discovery_precache_concurrency}
		min={1}
		max={8}
	/>
	<NumberField
		label="Discovery Delay"
		help="Pause after each artist (default: 0.2s)"
		bind:value={data.artist_discovery_precache_delay}
		min={0}
		max={5}
		step={0.1}
		unit="sec"
	/>
	<NumberField
		label="AudioDB Concurrency"
		help="TheAudioDB lookups at once (default: 4)"
		bind:value={data.audiodb_prewarm_concurrency}
		min={1}
		max={8}
	/>
	<NumberField
		label="AudioDB Delay"
		help="Pause before each lookup (default: 0.3s)"
		bind:value={data.audiodb_prewarm_delay}
		min={0}
		max={5}
		step={0.1}
		unit="sec"
	/>
</div>
<div class="divider my-4"></div>
<h4 class="font-medium text-sm text-base-content/70 mb-3">Library Image Refresh Limits</h4>
<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4">
	<NumberField
		label="Stall Timeout"
		help="Stop a refresh that makes no progress (default: 10 min)"
		bind:value={data.sync_stall_timeout_minutes}
		min={2}
		max={30}
		unit="min"
	/>
	<NumberField
		label="Max Timeout"
		help="Longest a refresh may run (default: 8 hrs)"
		bind:value={data.sync_max_timeout_hours}
		min={1}
		max={48}
		unit="hrs"
	/>
</div>
