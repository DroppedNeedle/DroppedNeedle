<script lang="ts">
	import SettingsPaneHeader from './SettingsPaneHeader.svelte';
	import { DEFAULT_SOURCE, isMusicSource, type MusicSource } from '$lib/stores/musicSource';
	import { getScrobblePreferencesQuery } from '$lib/features/profile/queries/scrobble-preferences.svelte';
	import { createUpdateScrobblePreferencesMutation } from '$lib/features/profile/queries/scrobble-preferences.svelte';
	import Card from '$lib/ui/Card.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';

	const preferencesQuery = getScrobblePreferencesQuery();
	const updatePreferences = createUpdateScrobblePreferencesMutation();

	let saving = $state(false);
	let message = $state('');

	const currentSource = $derived.by((): MusicSource => {
		const saved = preferencesQuery.data?.primary_music_source;
		return isMusicSource(saved) ? saved : DEFAULT_SOURCE;
	});

	async function handleChange(newSource: MusicSource) {
		if (newSource === currentSource) return;
		saving = true;
		message = '';
		const ok = await updatePreferences
			.mutateAsync({ primary_music_source: newSource })
			.then(() => true)
			.catch(() => false);
		if (ok) {
			message = 'Primary source saved';
			setTimeout(() => {
				message = '';
			}, 5000);
		} else {
			message = "Couldn't save your primary source.";
		}
		saving = false;
	}
</script>

<div class="space-y-5">
	<SettingsPaneHeader
		title="Primary Music Source"
		description="Your own source for Home and Discover, not a server default. The same setting as on your profile."
	/>
	<Card variant="bordered">
		<div class="max-w-xs">
			<SelectField
				label="Your primary source"
				bind:value={() => currentSource, (next) => void handleChange(next)}
				options={[
					{ value: 'listenbrainz', label: 'ListenBrainz' },
					{ value: 'lastfm', label: 'Last.fm' }
				]}
				help="Your Home and Discover use this source unless you switch it on the page."
				disabled={saving}
			/>
		</div>

		{#if saving}
			<div class="flex items-center gap-2 mt-2">
				<span class="loading loading-spinner loading-sm"></span>
				<span class="text-sm text-muted">Saving…</span>
			</div>
		{/if}

		{#if message}
			<div class="mt-2">
				<span class="text-sm {message.includes('Couldn') ? 'text-error' : 'text-success'}">
					{message}
				</span>
			</div>
		{/if}
	</Card>
</div>
