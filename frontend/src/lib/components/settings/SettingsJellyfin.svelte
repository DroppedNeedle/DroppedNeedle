<script lang="ts">
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { onDestroy } from 'svelte';
	import type { JellyfinConnectionSettings } from '$lib/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	type JellyfinUser = { id: string; name: string };
	type JellyfinTestResult = { success: boolean; message: string; users?: JellyfinUser[] };
	type JellyfinSettingsForm = ReturnType<typeof createSettingsForm<JellyfinConnectionSettings>> & {
		testResult: JellyfinTestResult | null;
	};

	const form = createSettingsForm<JellyfinConnectionSettings>({
		loadEndpoint: SETTINGS_ENDPOINTS.jellyfin(),
		saveEndpoint: SETTINGS_ENDPOINTS.jellyfin(),
		testEndpoint: SETTINGS_ENDPOINTS.jellyfinVerify(),
		enabledField: 'enabled',
		secondaryEnabledField: 'login_enabled',
		refreshIntegration: true,
		afterTest: (result) => {
			const typedResult = result as JellyfinTestResult;
			availableUsers = typedResult.users ?? [];
			if (typedResult.success && form.data && !form.data.user_id && availableUsers.length > 0) {
				form.data.user_id = availableUsers[0].id;
			}
		}
	}) as JellyfinSettingsForm;

	let availableUsers: JellyfinUser[] = $state([]);

	export async function load() {
		await form.load();
	}

	async function save() {
		await form.save();
	}

	async function test() {
		availableUsers = [];
		await form.test();
	}

	$effect(() => {
		form.load();
	});

	onDestroy(() => form.cleanup());
</script>

<Card title="Jellyfin Connection">
	<p class="text-base-content/70 mb-4">
		Connect your Jellyfin server for recently played tracks and favorites.
	</p>

	{#if form.loading}
		<div class="flex justify-center items-center py-12">
			<span class="loading loading-spinner loading-lg"></span>
		</div>
	{:else if form.data}
		<div class="space-y-4">
			<TextField
				label="Jellyfin URL"
				type="url"
				placeholder="http://localhost:8096"
				bind:value={form.data.jellyfin_url}
			/>

			<SecretField
				label="API Key"
				mask={SECRET_MASK}
				placeholder="Your Jellyfin API key"
				help="Dashboard → API Keys → Create"
				bind:value={form.data.api_key}
			/>

			{#if availableUsers.length > 0}
				<SelectField
					label="User"
					bind:value={form.data.user_id}
					options={availableUsers.map((user) => ({ value: user.id, label: user.name }))}
				/>
			{:else}
				<TextField
					label="User ID"
					placeholder="Test connection to select user"
					help="Test connection to load available users"
					bind:value={form.data.user_id}
				/>
			{/if}

			{#if form.testResult}
				<div
					class="alert"
					class:alert-success={form.testResult.success}
					class:alert-error={!form.testResult.success}
				>
					<span>{form.testResult.message}</span>
				</div>
			{/if}

			<ToggleRow
				label="Enable Jellyfin Integration"
				description={!form.testResult?.success && !form.wasAlreadyEnabled
					? 'Test connection first to enable'
					: 'Show recently played and favorite artists on the home page'}
				bind:checked={form.data.enabled}
				disabled={!form.testResult?.success && !form.wasAlreadyEnabled}
			/>

			<ToggleRow
				label="Allow login with Jellyfin"
				description={!form.testResult?.success && !form.wasAlreadySecondaryEnabled
					? 'Test and get a valid connection to enable'
					: 'Let users sign in to DroppedNeedle with their Jellyfin account'}
				bind:checked={form.data.login_enabled}
				disabled={!form.testResult?.success && !form.wasAlreadySecondaryEnabled}
			/>

			{#if form.message}
				<div
					class="alert"
					class:alert-success={form.messageType === 'success'}
					class:alert-error={form.messageType === 'error'}
				>
					<span>{form.message}</span>
				</div>
			{/if}

			<div class="flex justify-end gap-2 pt-2">
				<Button
					variant="ghost"
					loading={form.testing}
					disabled={!form.data.jellyfin_url || !form.data.api_key}
					onclick={test}
				>
					Test Connection
				</Button>
				<Button variant="primary" loading={form.saving} onclick={save}>Save Settings</Button>
			</div>
		</div>
	{/if}
</Card>
