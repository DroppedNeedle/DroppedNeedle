<script lang="ts">
	import { createMutation, createQuery } from '@tanstack/svelte-query';
	import { api } from '$lib/api/client';
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import type { JellyfinSettings } from '$lib/queries/settings/JellyfinSettingsQueries.svelte';
	import QueryBoundary from './QueryBoundary.svelte';
	import SaveBar from './SaveBar.svelte';
	import SecretField from './SecretField.svelte';
	import TextField from './TextField.svelte';
	import UnsavedChangesGuard from './UnsavedChangesGuard.svelte';
	import { SECRET_MASK } from './secretMask';
	import { confirmLeave, createSettingsSection } from './settingsSection.svelte';

	const section = createSettingsSection(
		createQuery(() => ({
			queryKey: ['settings-section-harness'],
			queryFn: ({ signal }) => api.global.v3.GET(SETTINGS_ENDPOINTS.jellyfin(), { signal })
		})),
		createMutation(() => ({
			mutationFn: (body: JellyfinSettings) => api.global.v3.PUT(SETTINGS_ENDPOINTS.jellyfin(), body)
		}))
	);

	let left = $state(false);
</script>

<QueryBoundary query={section} what="the harness settings">
	{#snippet children(draft)}
		<TextField label="Server URL" bind:value={draft.jellyfin_url} />
		<SecretField label="API key" mask={SECRET_MASK} bind:value={draft.api_key} />
		<SaveBar {...section.bar} />
	{/snippet}
</QueryBoundary>
<button type="button" onclick={() => confirmLeave(() => (left = true))}>Other tab</button>
<p>{left ? 'Left the tab' : 'On the tab'}</p>
<UnsavedChangesGuard />
