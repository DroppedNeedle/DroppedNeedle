<script lang="ts">
	import type { AdvancedSettingsForm } from './advanced-settings-types';
	import NumberField from '$lib/ui/NumberField.svelte';

	let { data = $bindable() }: { data: AdvancedSettingsForm } = $props();
</script>

<div class="alert alert-info alert-soft mb-4">
	<span class="text-sm"
		>Choose how long pages stay fresh in your browser. Lower values check for updates sooner. Higher
		values can make repeat visits feel faster.</span
	>
</div>
<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4 pt-2">
	<NumberField
		label="Home Page"
		help="Default: 5 minutes"
		bind:value={data.frontend_ttl_home}
		min={1}
		max={60}
		unit="min"
	/>
	<NumberField
		label="Discover Page"
		help="Default: 30 minutes"
		bind:value={data.frontend_ttl_discover}
		min={1}
		max={1440}
		unit="min"
	/>
	<NumberField
		label="Library"
		help="Default: 5 minutes"
		bind:value={data.frontend_ttl_library}
		min={1}
		max={60}
		unit="min"
	/>
	<NumberField
		label="Recently Added"
		help="Default: 5 minutes"
		bind:value={data.frontend_ttl_recently_added}
		min={1}
		max={60}
		unit="min"
	/>
	<NumberField
		label="Discover Queue"
		help="Default: 24 hours"
		bind:value={data.frontend_ttl_discover_queue}
		min={60}
		max={10080}
		unit="min"
	/>
	<NumberField
		label="Search & Discovery"
		help="Default: 5 minutes"
		bind:value={data.frontend_ttl_search}
		min={1}
		max={60}
		unit="min"
	/>
	<NumberField
		label="Local Files Sidebar"
		help="Default: 2 minutes"
		bind:value={data.frontend_ttl_local_files_sidebar}
		min={1}
		max={60}
		unit="min"
	/>
	<NumberField
		label="Jellyfin Sidebar"
		help="Default: 2 minutes"
		bind:value={data.frontend_ttl_jellyfin_sidebar}
		min={1}
		max={60}
		unit="min"
	/>
	<NumberField
		label="Plex Sidebar"
		help="Default: 2 minutes"
		bind:value={data.frontend_ttl_plex_sidebar}
		min={1}
		max={60}
		unit="min"
	/>
	<NumberField
		label="Playlist Sources"
		help="Default: 15 minutes"
		bind:value={data.frontend_ttl_playlist_sources}
		min={1}
		max={60}
		unit="min"
	/>
</div>
