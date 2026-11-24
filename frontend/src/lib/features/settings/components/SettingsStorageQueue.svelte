<script lang="ts">
	import type { AdvancedSettings } from './advanced-settings-types';
	import NumberField from '$lib/ui/NumberField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	let {
		data = $bindable(),
		fieldError
	}: {
		data: AdvancedSettings;
		fieldError: (field: string, label: string) => string | null;
	} = $props();
</script>

<div class="alert alert-info alert-soft mb-4">
	<span class="text-sm"
		>Controls how the Discover Queue is built in the background so it's ready when you open
		Discover.</span
	>
</div>
<h4 class="font-medium text-sm text-muted mb-3">Background Generation</h4>
<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4 pt-2">
	<NumberField
		label="Queue Size"
		error={fieldError('discover_queue_size', 'Queue Size')}
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
<h4 class="font-medium text-sm text-muted mb-3">Queue Tuning</h4>
<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4">
	<NumberField
		label="Last.fm MBID Lookups"
		error={fieldError('discover_queue_lastfm_mbid_max_lookups', 'Last.fm MBID Lookups')}
		help="Max MusicBrainz lookups when matching Last.fm albums (default: 10)"
		bind:value={data.discover_queue_lastfm_mbid_max_lookups}
		min={1}
		max={50}
	/>
</div>
<div class="divider my-4"></div>
<h4 class="font-medium text-sm text-muted mb-3">Listening Services</h4>
<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4">
	<ToggleRow
		label="Blend Last.fm and ListenBrainz"
		description="Use both when a user links both"
		bind:checked={data.discover_blend_listening_services}
	/>
</div>
