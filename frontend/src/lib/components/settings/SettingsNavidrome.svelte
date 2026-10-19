<script lang="ts">
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { onDestroy } from 'svelte';
	import { api, ApiError } from '$lib/api/client';
	import type { NavidromeConnectionSettings, NavidromePlaylistSyncResult } from '$lib/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	type NavidromeTestResult = { valid: boolean; message: string };
	type NavidromeSettingsForm = ReturnType<
		typeof createSettingsForm<NavidromeConnectionSettings>
	> & {
		testResult: NavidromeTestResult | null;
	};

	const form = createSettingsForm<NavidromeConnectionSettings>({
		loadEndpoint: SETTINGS_ENDPOINTS.navidrome(),
		saveEndpoint: SETTINGS_ENDPOINTS.navidrome(),
		testEndpoint: SETTINGS_ENDPOINTS.navidromeVerify(),
		enabledField: 'enabled',
		refreshIntegration: true
	}) as NavidromeSettingsForm;

	let syncing = $state(false);
	let syncResult = $state<NavidromePlaylistSyncResult | null>(null);

	export async function load() {
		await form.load();
	}

	async function syncPlaylists() {
		syncing = true;
		syncResult = null;
		try {
			// The endpoint reads saved settings, so save first or an edited
			// path or scope is ignored.
			await form.save();
			syncResult = await api.global.v3.POST(SETTINGS_ENDPOINTS.navidromePlaylistSync());
		} catch (error) {
			syncResult = {
				success: false,
				message:
					error instanceof ApiError
						? error.message
						: 'Playlist sync failed. Check the DroppedNeedle logs.',
				written: 0,
				unchanged: 0,
				removed: 0,
				removal_failures: 0,
				skipped_empty: 0,
				skipped_not_ours: 0,
				tracks_missing_files: 0,
				tracks_unrepresentable: 0
			};
		} finally {
			syncing = false;
		}
	}

	async function save() {
		await form.save();
	}

	async function test() {
		await form.test();
	}

	$effect(() => {
		form.load();
	});

	onDestroy(() => form.cleanup());
</script>

<Card title="Navidrome Connection">
	<p class="text-base-content/70 mb-4">
		Connect your Navidrome server for music streaming, recently played tracks, and favorites.
	</p>

	{#if form.loading}
		<div class="flex justify-center items-center py-12">
			<span class="loading loading-spinner loading-lg"></span>
		</div>
	{:else if form.data}
		<div class="space-y-4">
			<TextField
				label="Navidrome URL"
				type="url"
				placeholder="http://localhost:4533"
				bind:value={form.data.navidrome_url}
			/>

			<TextField
				label="Username"
				placeholder="Your Navidrome username"
				bind:value={form.data.username}
			/>

			<SecretField
				label="Password"
				mask={SECRET_MASK}
				placeholder="Your Navidrome password"
				bind:value={form.data.password}
			/>

			{#if form.testResult}
				<div
					class="alert"
					class:alert-success={form.testResult.valid}
					class:alert-error={!form.testResult.valid}
				>
					<span>{form.testResult.message}</span>
				</div>
			{/if}

			<ToggleRow
				label="Enable Navidrome Integration"
				description={!form.testResult?.valid && !form.wasAlreadyEnabled
					? 'Test connection first to enable'
					: 'Stream music, view recently played, and browse your Navidrome library'}
				bind:checked={form.data.enabled}
				disabled={!form.testResult?.valid && !form.wasAlreadyEnabled}
			/>

			<div class="divider"></div>

			<div>
				<h3 class="font-medium">Playlist Sync</h3>
				<p class="text-xs text-base-content/50 mt-1 whitespace-normal">
					Sync DroppedNeedle playlists to Navidrome - DroppedNeedle writes an
					<code>.m3u8</code> file per playlist into a folder Navidrome scans, and refreshes them in the
					background as playlists change.
				</p>
			</div>

			<ToggleRow
				label="Export playlists to Navidrome"
				description="Nothing is written until this is on and the settings are saved"
				bind:checked={form.data.playlist_sync_enabled}
			/>

			{#if form.data.playlist_sync_enabled}
				<TextField
					label="Playlist folder"
					placeholder="/music/playlists"
					help="An absolute path inside the DroppedNeedle container that must sit inside the same music library tree Navidrome scans. Track paths are written relative to this folder, so both apps have to see the same folder-to-track relationship."
					bind:value={form.data.playlist_sync_path}
				/>

				<SelectField
					label="Which playlists"
					bind:value={form.data.playlist_sync_scope}
					options={[
						{ value: 'public', label: 'Public playlists only' },
						{ value: 'all', label: 'All playlists' }
					]}
					help="Navidrome does not scope an imported playlist to a user, so anything exported is visible to everyone on that server. &quot;All playlists&quot; therefore publishes other users' private playlists too. To have them correctly scoped to a user on Navidrome, imported playlists can be assigned to a user (and made private or public) within the Navidrome dashboard."
				/>

				<ToggleRow
					label="Remove exported files when a playlist is removed from DroppedNeedle, or when a playlist is made private and only public playlists are exported"
					description="Only files exported from DroppedNeedle are ever removed. Other playlists in the same folder are never removed."
					bind:checked={form.data.playlist_sync_remove_deleted}
				/>

				{#if syncResult}
					<div
						class="alert"
						class:alert-success={syncResult.success}
						class:alert-error={!syncResult.success}
					>
						<div class="min-w-0 whitespace-normal break-words">
							<span>{syncResult.message}</span>
							{#if syncResult.removal_failures}
								<p class="text-xs mt-1">
									Files that could not be removed are still visible in Navidrome. They will be
									retried on the next sync. Check the folder's permissions if this persists.
								</p>
							{/if}
						</div>
					</div>
				{/if}

				<div class="flex justify-start">
					<Button
						variant="outline"
						size="sm"
						loading={syncing}
						disabled={form.saving || !form.data.playlist_sync_path}
						onclick={syncPlaylists}
					>
						Sync Now
					</Button>
				</div>
			{/if}

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
					disabled={!form.data.navidrome_url || !form.data.username || !form.data.password}
					onclick={test}
				>
					Test Connection
				</Button>
				<Button variant="primary" loading={form.saving} onclick={save}>Save Settings</Button>
			</div>
		</div>
	{/if}
</Card>
