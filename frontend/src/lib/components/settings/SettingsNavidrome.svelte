<script lang="ts">
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { onDestroy } from 'svelte';
	import { api, ApiError } from '$lib/api/client';
	import type { NavidromeConnectionSettings, NavidromePlaylistSyncResult } from '$lib/types';

	type NavidromeTestResult = { valid: boolean; message: string };
	type NavidromeSettingsForm = ReturnType<
		typeof createSettingsForm<NavidromeConnectionSettings>
	> & {
		testResult: NavidromeTestResult | null;
	};

	const form = createSettingsForm<NavidromeConnectionSettings>({
		loadEndpoint: '/api/v1/settings/navidrome',
		saveEndpoint: '/api/v1/settings/navidrome',
		testEndpoint: '/api/v1/settings/navidrome/verify',
		enabledField: 'enabled',
		refreshIntegration: true
	}) as NavidromeSettingsForm;

	let showPassword = $state(false);
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
			syncResult = await api.post<NavidromePlaylistSyncResult>(
				'/api/v1/settings/navidrome/playlist-sync'
			);
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

<div class="card bg-base-200">
	<div class="card-body">
		<h2 class="card-title text-2xl">Navidrome Connection</h2>
		<p class="text-base-content/70 mb-4">
			Connect your Navidrome server for music streaming, recently played tracks, and favorites.
		</p>

		{#if form.loading}
			<div class="flex justify-center items-center py-12">
				<span class="loading loading-spinner loading-lg"></span>
			</div>
		{:else if form.data}
			<div class="space-y-4">
				<div class="form-control w-full">
					<label class="label" for="navidrome-url">
						<span class="label-text">Navidrome URL</span>
					</label>
					<input
						id="navidrome-url"
						type="url"
						bind:value={form.data.navidrome_url}
						class="input input-bordered w-full"
						placeholder="http://localhost:4533"
					/>
				</div>

				<div class="form-control w-full">
					<label class="label" for="navidrome-username">
						<span class="label-text">Username</span>
					</label>
					<input
						id="navidrome-username"
						type="text"
						bind:value={form.data.username}
						class="input input-bordered w-full"
						placeholder="Your Navidrome username"
					/>
				</div>

				<div class="form-control w-full">
					<label class="label" for="navidrome-password">
						<span class="label-text">Password</span>
					</label>
					<div class="join w-full">
						<input
							id="navidrome-password"
							type={showPassword ? 'text' : 'password'}
							bind:value={form.data.password}
							class="input input-bordered join-item flex-1"
							placeholder="Your Navidrome password"
						/>
						<button
							type="button"
							class="btn join-item"
							onclick={() => (showPassword = !showPassword)}
						>
							{showPassword ? 'Hide' : 'Show'}
						</button>
					</div>
				</div>

				{#if form.testResult}
					<div
						class="alert"
						class:alert-success={form.testResult.valid}
						class:alert-error={!form.testResult.valid}
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
							disabled={!form.testResult?.valid && !form.wasAlreadyEnabled}
						/>
						<div>
							<span class="label-text font-medium">Enable Navidrome Integration</span>
							<p class="text-xs text-base-content/50">
								{#if !form.testResult?.valid && !form.wasAlreadyEnabled}
									Test connection first to enable
								{:else}
									Stream music, view recently played, and browse your Navidrome library
								{/if}
							</p>
						</div>
					</label>
				</div>

				<div class="divider"></div>

				<div>
					<h3 class="font-medium">Playlist Sync</h3>
					<p class="text-xs text-base-content/50 mt-1 whitespace-normal">
						Sync DroppedNeedle playlists to Navidrome - DroppedNeedle writes an
						<code>.m3u8</code> file per playlist into a folder Navidrome scans, and refreshes them in
						the background as playlists change.
					</p>
				</div>

				<div class="form-control">
					<label class="label cursor-pointer justify-start gap-4">
						<input
							type="checkbox"
							bind:checked={form.data.playlist_sync_enabled}
							class="toggle toggle-primary"
						/>
						<div class="whitespace-normal">
							<span class="label-text font-medium">Export playlists to Navidrome</span>
							<p class="text-xs text-base-content/50">
								Nothing is written until this is on and the settings are saved
							</p>
						</div>
					</label>
				</div>

				{#if form.data.playlist_sync_enabled}
					<div class="form-control w-full">
						<label class="label" for="navidrome-playlist-path">
							<span class="label-text">Playlist folder</span>
						</label>
						<input
							id="navidrome-playlist-path"
							type="text"
							bind:value={form.data.playlist_sync_path}
							class="input input-bordered w-full"
							placeholder="/music/playlists"
						/>
						<div class="label whitespace-normal">
							<span class="label-text-alt text-base-content/50">
								An absolute path <em>inside the DroppedNeedle container</em> that must sit
								<strong>inside the same music library tree Navidrome scans</strong> — track paths are
								written relative to this folder, so both apps have to see the same folder-to-track relationship.
							</span>
						</div>
					</div>

					<div class="form-control w-full">
						<label class="label" for="navidrome-playlist-scope">
							<span class="label-text">Which playlists</span>
						</label>
						<select
							id="navidrome-playlist-scope"
							bind:value={form.data.playlist_sync_scope}
							class="select select-bordered w-full"
						>
							<option value="public">Public playlists only</option>
							<option value="all">All playlists</option>
						</select>
						<div class="label whitespace-normal">
							<span class="label-text-alt text-base-content/50">
								Navidrome does not scope an imported playlist to a user, so anything exported is
								visible to everyone on that server. "All playlists" therefore publishes other users'
								private playlists too. To have them correctly scoped to a user on Navidrome,
								imported playlists can be assigned to a user (and made private or public) within the
								Navidrome dashboard.
							</span>
						</div>
					</div>

					<div class="form-control">
						<label class="label cursor-pointer justify-start gap-4">
							<input
								type="checkbox"
								bind:checked={form.data.playlist_sync_remove_deleted}
								class="toggle toggle-primary"
							/>
							<div class="whitespace-normal">
								<span class="label-text font-medium">
									Remove exported files when a playlist is removed from DroppedNeedle, or when a
									playlist is made private and only public playlists are exported
								</span>
								<p class="text-xs text-base-content/50">
									Only files exported from DroppedNeedle are ever removed. Other playlists in the
									same folder are never removed.
								</p>
							</div>
						</label>
					</div>

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
										retried on the next sync — check the folder's permissions if this persists.
									</p>
								{/if}
							</div>
						</div>
					{/if}

					<div class="flex justify-start">
						<button
							type="button"
							class="btn btn-outline btn-sm"
							onclick={syncPlaylists}
							disabled={syncing || form.saving || !form.data.playlist_sync_path}
						>
							{#if syncing}
								<span class="loading loading-spinner loading-sm"></span>
							{/if}
							Sync Now
						</button>
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
					<button
						type="button"
						class="btn btn-ghost"
						onclick={test}
						disabled={form.testing ||
							!form.data.navidrome_url ||
							!form.data.username ||
							!form.data.password}
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
