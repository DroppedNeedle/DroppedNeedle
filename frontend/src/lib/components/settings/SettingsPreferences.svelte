<script lang="ts">
	import { preferencesStore } from '$lib/stores/preferences';
	import type { UserPreferences, ReleaseTypeOption } from '$lib/types';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import { ArtistQueryKeyFactory } from '$lib/queries/artist/ArtistQueryKeyFactory';

	let preferences: UserPreferences = $state({
		primary_types: [],
		secondary_types: []
	});
	let saving = $state(false);
	let saveMessage = $state('');

	const primaryTypes: ReleaseTypeOption[] = [
		{ id: 'album', title: 'Album', description: 'Release groups MusicBrainz classifies as albums' },
		{ id: 'ep', title: 'EP', description: 'Release groups MusicBrainz classifies as EPs' },
		{
			id: 'single',
			title: 'Single',
			description: 'Release groups MusicBrainz classifies as singles'
		},
		{
			id: 'broadcast',
			title: 'Broadcast',
			description: 'Release groups MusicBrainz classifies as broadcasts'
		},
		{
			id: 'other',
			title: 'Other',
			description: 'Release groups MusicBrainz classifies as Other'
		}
	];

	const secondaryTypes: ReleaseTypeOption[] = [
		{ id: 'studio', title: 'Studio', description: 'Release groups with no listed secondary type' },
		{
			id: 'compilation',
			title: 'Compilation',
			description: 'Release groups tagged by MusicBrainz as compilations'
		},
		{
			id: 'soundtrack',
			title: 'Soundtrack',
			description: 'Release groups tagged by MusicBrainz as soundtracks'
		},
		{
			id: 'spokenword',
			title: 'Spoken Word',
			description: 'Release groups tagged by MusicBrainz as spoken word'
		},
		{
			id: 'interview',
			title: 'Interview',
			description: 'Release groups tagged by MusicBrainz as interviews'
		},
		{
			id: 'audio drama',
			title: 'Audio Drama',
			description: 'Release groups tagged by MusicBrainz as audio dramas'
		},
		{ id: 'live', title: 'Live', description: 'Release groups tagged by MusicBrainz as live' },
		{ id: 'remix', title: 'Remix', description: 'Release groups tagged by MusicBrainz as remixes' },
		{
			id: 'dj-mix',
			title: 'DJ-mix',
			description: 'Release groups tagged by MusicBrainz as DJ mixes'
		},
		{
			id: 'mixtape/street',
			title: 'Mixtape/Street',
			description: 'Release groups tagged by MusicBrainz as mixtape or street'
		},
		{ id: 'demo', title: 'Demo', description: 'Release groups tagged by MusicBrainz as demos' }
	];

	function toggleType(category: 'primary_types' | 'secondary_types', id: string) {
		const index = preferences[category].indexOf(id);
		if (index > -1) {
			preferences[category] = preferences[category].filter((t) => t !== id);
		} else {
			preferences[category] = [...preferences[category], id];
		}
	}

	async function handleSave() {
		saving = true;
		saveMessage = '';

		const success = await preferencesStore.save(preferences);

		if (success) {
			saveMessage = 'Saved. Artist pages and search results will refresh automatically.';

			// these prefs change which releases show on artist pages + search
			await invalidateQueriesWithPersister({ queryKey: ArtistQueryKeyFactory.prefix });
			window.dispatchEvent(new CustomEvent('search-refresh'));

			setTimeout(() => {
				saveMessage = '';
			}, 5000);
		} else {
			saveMessage = "Couldn't save your settings. Please try again.";
		}

		saving = false;
	}

	$effect(() => {
		preferencesStore.load();
		const unsubscribe = preferencesStore.subscribe((prefs) => {
			preferences = { ...prefs };
		});
		return unsubscribe;
	});
</script>

{#snippet typeTable(types: ReleaseTypeOption[], category: 'primary_types' | 'secondary_types')}
	<div class="overflow-x-auto">
		<table class="table">
			<thead>
				<tr>
					<th class="w-12 text-center">
						<span class="text-xs opacity-60">Show</span>
					</th>
					<th>Type</th>
					<th class="hidden sm:table-cell">Description</th>
				</tr>
			</thead>
			<tbody>
				{#each types as type (type.id)}
					{@const msEnabled = preferences[category].includes(type.id)}
					<tr>
						<td class="w-12 text-center">
							<input
								type="checkbox"
								class="checkbox checkbox-primary checkbox-sm"
								checked={msEnabled}
								onchange={() => toggleType(category, type.id)}
							/>
						</td>
						<td class="font-medium">{type.title}</td>
						<td class="text-base-content/70 hidden sm:table-cell">{type.description}</td>
					</tr>
				{/each}
			</tbody>
		</table>
	</div>
{/snippet}

<div class="card bg-base-200">
	<div class="card-body">
		<h2 class="card-title text-2xl mb-2">Release Types</h2>
		<p class="text-base-content/70 mb-6">
			Choose which release types appear on artist pages and in search. The same choices apply to
			automatic downloads for followed artists. Saving a different selection starts a new baseline,
			so releases already present are not queued.
		</p>

		<div class="mb-8">
			<h3 class="text-xl font-semibold mb-1">Primary Types</h3>
			<p class="text-base-content/60 mb-4 text-sm">
				Choose the main MusicBrainz release formats to include.
			</p>
			{@render typeTable(primaryTypes, 'primary_types')}
		</div>

		<div class="mb-8">
			<h3 class="text-xl font-semibold mb-1">Secondary Types</h3>
			<p class="text-base-content/60 mb-4 text-sm">
				Choose which additional release classifications to include.
			</p>
			{@render typeTable(secondaryTypes, 'secondary_types')}
		</div>

		<div class="card-actions justify-end items-center gap-4">
			{#if saveMessage}
				<div
					class="alert flex-1"
					class:alert-success={saveMessage.includes('success')}
					class:alert-error={saveMessage.includes('Failed')}
				>
					<span>{saveMessage}</span>
				</div>
			{/if}
			<button class="btn btn-primary" onclick={handleSave} disabled={saving}>
				{#if saving}
					<span class="loading loading-spinner loading-sm"></span>
					Saving...
				{:else}
					Save Settings
				{/if}
			</button>
		</div>
	</div>
</div>
