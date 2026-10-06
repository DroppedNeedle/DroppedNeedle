<script lang="ts">
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { onDestroy } from 'svelte';
	import type { JellyfinConnectionSettings } from '$lib/types';

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

	let showApiKey = $state(false);
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

<div class="card bg-base-200">
	<div class="card-body">
		<h2 class="card-title text-2xl">Jellyfin Connection</h2>
		<p class="text-base-content/70 mb-4">
			Connect your Jellyfin server for recently played tracks and favorites.
		</p>

		{#if form.loading}
			<div class="flex justify-center items-center py-12">
				<span class="loading loading-spinner loading-lg"></span>
			</div>
		{:else if form.data}
			<div class="space-y-4">
				<div class="form-control w-full">
					<label class="label" for="jellyfin-url">
						<span class="label-text">Jellyfin URL</span>
					</label>
					<input
						id="jellyfin-url"
						type="url"
						bind:value={form.data.jellyfin_url}
						class="input input-bordered w-full"
						placeholder="http://localhost:8096"
					/>
				</div>

				<div class="form-control w-full">
					<label class="label" for="jellyfin-api-key">
						<span class="label-text">API Key</span>
					</label>
					<div class="join w-full">
						<input
							id="jellyfin-api-key"
							type={showApiKey ? 'text' : 'password'}
							bind:value={form.data.api_key}
							class="input input-bordered join-item flex-1"
							placeholder="Your Jellyfin API key"
						/>
						<button type="button" class="btn join-item" onclick={() => (showApiKey = !showApiKey)}>
							{showApiKey ? 'Hide' : 'Show'}
						</button>
					</div>
					<label class="label" for="jellyfin-api-key">
						<span class="label-text-alt text-base-content/50">Dashboard → API Keys → Create</span>
					</label>
				</div>

				{#if availableUsers.length > 0}
					<div class="form-control w-full">
						<label class="label" for="jellyfin-user">
							<span class="label-text">User</span>
						</label>
						<select
							id="jellyfin-user"
							bind:value={form.data.user_id}
							class="select select-bordered w-full"
						>
							{#each availableUsers as user (user.id)}
								<option value={user.id}>{user.name}</option>
							{/each}
						</select>
					</div>
				{:else}
					<div class="form-control w-full">
						<label class="label" for="jellyfin-user-id">
							<span class="label-text">User ID</span>
						</label>
						<input
							id="jellyfin-user-id"
							type="text"
							bind:value={form.data.user_id}
							class="input input-bordered w-full"
							placeholder="Test connection to select user"
						/>
						<label class="label" for="jellyfin-user-id">
							<span class="label-text-alt text-base-content/50">
								Test connection to load available users
							</span>
						</label>
					</div>
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

				<div class="form-control">
					<label class="label cursor-pointer justify-start gap-4">
						<input
							type="checkbox"
							bind:checked={form.data.enabled}
							class="toggle toggle-primary"
							disabled={!form.testResult?.success && !form.wasAlreadyEnabled}
						/>
						<div>
							<span class="label-text font-medium">Enable Jellyfin Integration</span>
							<p class="text-xs text-base-content/50">
								{#if !form.testResult?.success && !form.wasAlreadyEnabled}
									Test connection first to enable
								{:else}
									Show recently played and favorite artists on the home page
								{/if}
							</p>
						</div>
					</label>
				</div>

				<div class="form-control">
					<label class="label cursor-pointer justify-start gap-4">
						<input
							type="checkbox"
							bind:checked={form.data.login_enabled}
							class="toggle toggle-primary"
							disabled={!form.testResult?.success && !form.wasAlreadySecondaryEnabled}
						/>
						<div>
							<span class="label-text font-medium">Allow login with Jellyfin</span>
							<p class="text-xs text-base-content/50">
								{#if !form.testResult?.success && !form.wasAlreadySecondaryEnabled}
									Test and get a valid connection to enable
								{:else}
									Let users sign in to DroppedNeedle with their Jellyfin account
								{/if}
							</p>
						</div>
					</label>
				</div>

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
					<button
						type="button"
						class="btn btn-ghost"
						onclick={test}
						disabled={form.testing || !form.data.jellyfin_url || !form.data.api_key}
					>
						{#if form.testing}
							<span class="loading loading-spinner loading-sm"></span>
						{/if}
						Test Connection
					</button>
					<button type="button" class="btn btn-primary" onclick={save} disabled={form.saving}>
						{#if form.saving}
							<span class="loading loading-spinner loading-sm"></span>
						{/if}
						Save Settings
					</button>
				</div>
			</div>
		{/if}
	</div>
</div>
