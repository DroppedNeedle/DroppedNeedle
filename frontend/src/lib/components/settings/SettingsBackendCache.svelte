<script lang="ts">
	import type { AdvancedSettingsForm } from './advanced-settings-types';
	import SettingsNumberField from './SettingsNumberField.svelte';

	let { data = $bindable() }: { data: AdvancedSettingsForm } = $props();
</script>

<div class="alert alert-info alert-soft mb-4">
	<span class="text-sm"
		>These settings control typed-view and existing disk metadata freshness. MusicBrainz display
		responses have a separate 24-hour disk freshness window and a 128 MiB limit. Rebuilding a view
		can reuse a fresh response without contacting the provider; it does not renew the response's
		age. Your current TTL settings and memory entry limits are preserved.</span
	>
</div>
<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4 pt-2">
	<SettingsNumberField
		label="Library Albums"
		description="Default: 24 hours"
		bind:value={data.cache_ttl_album_library}
		min={1}
		max={168}
		unit="hours"
	/>
	<SettingsNumberField
		label="Non-Library Albums"
		description="Default: 6 hours"
		bind:value={data.cache_ttl_album_non_library}
		min={1}
		max={24}
		unit="hours"
	/>
	<SettingsNumberField
		label="Library Artists"
		description="Default: 6 hours"
		bind:value={data.cache_ttl_artist_library}
		min={1}
		max={168}
		unit="hours"
	/>
	<SettingsNumberField
		label="Non-Library Artists"
		description="Default: 6 hours"
		bind:value={data.cache_ttl_artist_non_library}
		min={1}
		max={168}
		unit="hours"
	/>
	<SettingsNumberField
		label="Discovery (Library Artists)"
		description="Popular albums/songs/similar (default: 6h)"
		bind:value={data.cache_ttl_artist_discovery_library}
		min={1}
		max={168}
		unit="hours"
	/>
	<SettingsNumberField
		label="Discovery (Non-Library Artists)"
		description="Popular albums/songs/similar (default: 1h)"
		bind:value={data.cache_ttl_artist_discovery_non_library}
		min={1}
		max={168}
		unit="hours"
	/>
	<SettingsNumberField
		label="Search Results"
		description="Default: 60 minutes"
		bind:value={data.cache_ttl_search}
		min={1}
		max={1440}
		unit="min"
	/>
	<SettingsNumberField
		label="Persistent Metadata"
		description="Covers & enrichment (default: 24h)"
		bind:value={data.persistent_metadata_ttl_hours}
		min={1}
		max={168}
		unit="hours"
	/>
	<SettingsNumberField
		label="Jellyfin Recent (Backend)"
		description="Default: 5 minutes"
		bind:value={data.cache_ttl_jellyfin_recently_played}
		min={1}
		max={60}
		unit="min"
	/>
	<SettingsNumberField
		label="Jellyfin Favorites (Backend)"
		description="Default: 5 minutes"
		bind:value={data.cache_ttl_jellyfin_favorites}
		min={1}
		max={60}
		unit="min"
	/>
	<SettingsNumberField
		label="Jellyfin Genres (Backend)"
		description="Default: 60 minutes"
		bind:value={data.cache_ttl_jellyfin_genres}
		min={1}
		max={1440}
		unit="min"
	/>
	<SettingsNumberField
		label="Jellyfin Stats (Backend)"
		description="Default: 10 minutes"
		bind:value={data.cache_ttl_jellyfin_library_stats}
		min={1}
		max={60}
		unit="min"
	/>
	<SettingsNumberField
		label="Plex Albums (Backend)"
		description="Default: 5 minutes"
		bind:value={data.cache_ttl_plex_albums}
		min={1}
		max={60}
		unit="min"
	/>
	<SettingsNumberField
		label="Plex Search (Backend)"
		description="Default: 2 minutes"
		bind:value={data.cache_ttl_plex_search}
		min={1}
		max={60}
		unit="min"
	/>
	<SettingsNumberField
		label="Plex Genres (Backend)"
		description="Default: 60 minutes"
		bind:value={data.cache_ttl_plex_genres}
		min={1}
		max={1440}
		unit="min"
	/>
	<SettingsNumberField
		label="Plex Stats (Backend)"
		description="Default: 10 minutes"
		bind:value={data.cache_ttl_plex_stats}
		min={1}
		max={60}
		unit="min"
	/>
</div>
