<script lang="ts">
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import type { AdvancedSettings } from './advanced-settings-types';
	import SecretField from '$lib/ui/SecretField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	let { data = $bindable() }: { data: AdvancedSettings } = $props();
</script>

<div class="alert alert-info alert-soft mb-4">
	<span class="text-sm"
		>AudioDB provides rich artist and album images (fanart, banners, logos, CD art). The free tier
		allows 30 requests/minute. A premium API key removes this limit.</span
	>
</div>

<div class="grid grid-cols-1 sm:grid-cols-2 gap-x-6 gap-y-4 pt-2">
	<ToggleRow
		label="AudioDB Enabled"
		description="Fetch images from TheAudioDB"
		bind:checked={data.audiodb_enabled}
	/>

	<SecretField
		label="API Key"
		mask={SECRET_MASK}
		placeholder="Enter API key"
		help="Default: 123 (free tier, 30 req/min). Premium keys available at theaudiodb.com"
		bind:value={data.audiodb_api_key}
	/>

	<ToggleRow
		label="Name Search Fallback"
		description="Try artist/album name search when MusicBrainz ID lookup returns no images"
		bind:checked={data.audiodb_name_search_fallback}
	/>

	<ToggleRow
		label="Direct Remote Images"
		description="When enabled, your browser loads images directly from TheAudioDB's CDN (faster). Disable to route all images through DroppedNeedle (more private)."
		bind:checked={data.direct_remote_images_enabled}
	/>

	<ToggleRow
		label="Prefer Local Cover Art"
		description="When enabled, covers come from folder images (cover.jpg and similar) or art embedded in the audio files before any online source is checked. Local art still works when an image provider is down."
		bind:checked={data.prefer_local_cover_art}
	/>
</div>
