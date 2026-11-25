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

<h4 class="font-medium text-sm text-muted mb-3">Last.fm Matching</h4>
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
