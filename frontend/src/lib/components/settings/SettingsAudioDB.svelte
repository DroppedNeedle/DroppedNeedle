<script lang="ts">
	import type { AdvancedSettingsForm } from './advanced-settings-types';
	import SettingsNumberField from './SettingsNumberField.svelte';

	let { data = $bindable() }: { data: AdvancedSettingsForm } = $props();
</script>

<div class="alert alert-info alert-soft mb-4">
	<span class="text-sm"
		>AudioDB provides rich artist and album images (fanart, banners, logos, CD art). The free tier
		allows 30 requests/minute. A premium API key removes this limit.</span
	>
</div>

<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4 pt-2">
	<fieldset class="fieldset">
		<legend class="fieldset-legend">AudioDB Enabled</legend>
		<label
			class="label w-full cursor-pointer justify-between gap-3"
			data-testid="audiodb-toggle-enabled"
		>
			<span class="text-sm min-w-0">Fetch images from TheAudioDB</span>
			<input
				type="checkbox"
				bind:checked={data.audiodb_enabled}
				class="toggle toggle-primary shrink-0"
			/>
		</label>
	</fieldset>

	<fieldset class="fieldset">
		<legend class="fieldset-legend">API Key</legend>
		<label class="input w-full">
			<input
				type="password"
				bind:value={data.audiodb_api_key}
				placeholder="Enter API key"
				class="grow"
			/>
		</label>
		<p class="label text-base-content/50">
			Default: 123 (free tier, 30 req/min). Premium keys available at theaudiodb.com
		</p>
	</fieldset>

	<fieldset class="fieldset">
		<legend class="fieldset-legend">Name Search Fallback</legend>
		<label
			class="label w-full cursor-pointer justify-between gap-3"
			data-testid="audiodb-toggle-name-search"
		>
			<span class="text-sm min-w-0"
				>Try artist/album name search when MusicBrainz ID lookup returns no images</span
			>
			<input
				type="checkbox"
				bind:checked={data.audiodb_name_search_fallback}
				class="toggle toggle-primary shrink-0"
			/>
		</label>
	</fieldset>

	<fieldset class="fieldset">
		<legend class="fieldset-legend">Direct Remote Images</legend>
		<label
			class="label w-full cursor-pointer justify-between gap-3"
			data-testid="audiodb-toggle-direct-remote"
		>
			<span class="text-sm min-w-0">Load images directly from TheAudioDB's CDN</span>
			<input
				type="checkbox"
				bind:checked={data.direct_remote_images_enabled}
				class="toggle toggle-primary shrink-0"
			/>
		</label>
		<p class="label text-base-content/50">
			When enabled, your browser loads images directly from TheAudioDB's CDN (faster). Disable to
			route all images through DroppedNeedle (more private).
		</p>
	</fieldset>

	<fieldset class="fieldset">
		<legend class="fieldset-legend">Prefer Local Cover Art</legend>
		<label
			class="label w-full cursor-pointer justify-between gap-3"
			data-testid="audiodb-toggle-prefer-local"
		>
			<span class="text-sm min-w-0">Use cover art from your own files first</span>
			<input
				type="checkbox"
				bind:checked={data.prefer_local_cover_art}
				class="toggle toggle-primary shrink-0"
			/>
		</label>
		<p class="label text-base-content/50">
			When enabled, covers come from folder images (cover.jpg and similar) or art embedded in the
			audio files before any online source is checked. Local art still works when an image provider
			is down.
		</p>
	</fieldset>

	<SettingsNumberField
		label="Found Images TTL"
		description="Default: 168 hours (7 days)"
		bind:value={data.cache_ttl_audiodb_found}
		min={1}
		max={720}
		unit="hrs"
	/>

	<SettingsNumberField
		label="Not Found TTL"
		description="Default: 24 hours"
		bind:value={data.cache_ttl_audiodb_not_found}
		min={1}
		max={168}
		unit="hrs"
	/>

	<SettingsNumberField
		label="Library Items TTL"
		description="Default: 336 hours (14 days)"
		bind:value={data.cache_ttl_audiodb_library}
		min={24}
		max={720}
		unit="hrs"
	/>

	<SettingsNumberField
		label="Recently Viewed Bytes TTL"
		description="Default: 48 hours"
		bind:value={data.cache_ttl_recently_viewed_bytes}
		min={1}
		max={168}
		unit="hrs"
	/>
</div>
