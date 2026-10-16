<script lang="ts">
	import { musicSourceStore, type MusicSource } from '$lib/stores/musicSource';
	import { fromStore } from 'svelte/store';
	import Card from '$lib/ui/Card.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';

	const source = fromStore(musicSourceStore);

	let saving = $state(false);
	let message = $state('');

	const currentSource = $derived(source.current.source);

	async function handleChange(newSource: MusicSource) {
		if (newSource === currentSource) return;
		saving = true;
		message = '';
		const ok = await musicSourceStore.save(newSource);
		if (ok) {
			message = 'Default music source updated';
			setTimeout(() => {
				message = '';
			}, 5000);
		} else {
			message = "Couldn't save the default music source.";
		}
		saving = false;
	}

	$effect(() => {
		musicSourceStore.load();
	});
</script>

<Card title="Primary Music Source">
	<p class="text-base-content/70 mb-4">
		Choose which listening service powers your Home and Discover by default. You can also set this
		from your profile, and switch it on each page.
	</p>

	<div class="max-w-xs">
		<SelectField
			label="Default source for your discovery data"
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
			<span class="text-sm text-base-content/70">Saving…</span>
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
