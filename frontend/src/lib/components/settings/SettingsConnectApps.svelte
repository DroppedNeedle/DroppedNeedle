<script lang="ts">
	import { resolve } from '$app/paths';
	import { Trash2, TriangleAlert, Users, Waypoints } from 'lucide-svelte';

	import { toastStore } from '$lib/stores/toast';
	import {
		getAdminAppPasswordsQuery,
		getConnectAppsSettingsQuery
	} from '$lib/queries/connect-apps/ConnectAppsQueries.svelte';
	import {
		adminRevokeAppPassword,
		saveConnectAppsSettings
	} from '$lib/queries/connect-apps/ConnectAppsMutations.svelte';
	import type { AdminAppPasswordView, ConnectAppsSettings } from '$lib/types';

	const settingsQuery = getConnectAppsSettingsQuery();
	const rosterQuery = getAdminAppPasswordsQuery();
	const save = saveConnectAppsSettings();
	const adminRevoke = adminRevokeAppPassword();

	// seeded once so a background refetch never clobbers in-flight edits
	let subsonicEnabled = $state(false);
	let jellyfinEnabled = $state(false);
	let transcodingEnabled = $state(true);
	let transcodeFormat = $state<'mp3' | 'opus'>('mp3');
	let transcodeMaxKbps = $state(320);
	let discoverMode = $state<'local-only' | 'lazy-mb' | 'use-scrobble-targets'>('local-only');
	let seeded = $state(false);

	let pendingRevoke = $state<AdminAppPasswordView | null>(null);
	let revokeDialog = $state<HTMLDialogElement>();

	$effect(() => {
		if (!revokeDialog) return;
		if (pendingRevoke !== null) revokeDialog.showModal();
		else if (revokeDialog.open) revokeDialog.close();
	});

	$effect(() => {
		const d = settingsQuery.data;
		if (d && !seeded) {
			subsonicEnabled = d.subsonic_enabled;
			jellyfinEnabled = d.jellyfin_enabled;
			transcodingEnabled = d.transcoding_enabled;
			transcodeFormat = d.transcode_default_format;
			transcodeMaxKbps = d.transcode_max_bitrate_kbps;
			discoverMode = d.discover_mode;
			seeded = true;
		}
	});

	function currentSettings(): ConnectAppsSettings {
		return {
			subsonic_enabled: subsonicEnabled,
			jellyfin_enabled: jellyfinEnabled,
			transcoding_enabled: transcodingEnabled,
			transcode_default_format: transcodeFormat,
			// a cleared number input binds to null; fall back so we never PUT null
			transcode_max_bitrate_kbps: transcodeMaxKbps ?? 320,
			advertise_server_name: settingsQuery.data?.advertise_server_name ?? 'DroppedNeedle',
			advertise_server_version: settingsQuery.data?.advertise_server_version ?? '10.10.6',
			discover_mode: discoverMode
		};
	}

	async function handleSave() {
		try {
			await save.mutateAsync(currentSettings());
			toastStore.show({ message: 'Connect Apps settings saved', type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not save settings', type: 'error' });
		}
	}

	async function confirmRevoke() {
		const target = pendingRevoke;
		if (!target) return;
		try {
			await adminRevoke.mutateAsync({ id: target.id, userId: target.user_id });
			toastStore.show({ message: `Revoked "${target.name}"`, type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not revoke app-password', type: 'error' });
		} finally {
			pendingRevoke = null;
		}
	}

	function formatDate(iso: string | null): string {
		if (!iso) return 'Never';
		const d = new Date(iso);
		return Number.isNaN(d.getTime()) ? '-' : d.toLocaleDateString();
	}

	const rosterCount = $derived(rosterQuery.data?.active_count ?? 0);
</script>

<section class="flex flex-col gap-6">
	<header class="overflow-hidden rounded-box border border-accent/20 bg-base-200 p-6">
		<div class="flex items-center gap-2 text-accent">
			<Waypoints class="h-5 w-5" aria-hidden="true" />
			<h2 class="text-xl font-bold tracking-tight">Connect Apps</h2>
		</div>
		<p class="mt-1 max-w-2xl text-base-content/70">
			Let music apps like Symfonium and Finamp stream this library over the OpenSubsonic or Jellyfin
			protocols.
		</p>
		<p class="mt-1 max-w-2xl text-sm text-base-content/50">
			These are server-wide switches. Each person creates their own app-password from their
			<a href={resolve('/profile#connect-apps')} class="link link-accent">Profile → Connect Apps</a
			>.
		</p>
	</header>

	{#if settingsQuery.isLoading}
		<div class="skeleton h-56 w-full rounded-box"></div>
	{:else if settingsQuery.isError}
		<div class="alert alert-error" role="alert">
			<TriangleAlert class="h-5 w-5" aria-hidden="true" />
			<span>Could not load Connect Apps settings.</span>
		</div>
	{:else}
		<div class="card border border-base-300 bg-base-200">
			<div class="card-body gap-5">
				<h3 class="card-title text-base">Protocols</h3>

				<label class="flex items-center justify-between gap-4">
					<span>
						<span class="font-medium">OpenSubsonic API</span>
						<span class="block text-sm text-base-content/60">
							Symfonium, Feishin, Amperfy and other Subsonic apps.
						</span>
					</span>
					<input
						type="checkbox"
						class="toggle toggle-accent"
						aria-label="Enable OpenSubsonic API"
						bind:checked={subsonicEnabled}
					/>
				</label>

				<label class="flex items-center justify-between gap-4">
					<span>
						<span class="font-medium">Jellyfin-compatible API</span>
						<span class="block text-sm text-base-content/60">
							Finamp, Jellify, Manet Music and Symfonium's Jellyfin mode.
						</span>
					</span>
					<input
						type="checkbox"
						class="toggle toggle-accent"
						aria-label="Enable Jellyfin API"
						bind:checked={jellyfinEnabled}
					/>
				</label>

				<div class="divider my-0"></div>

				<label class="flex items-center justify-between gap-4">
					<span class="font-medium">On-the-fly transcoding</span>
					<input
						type="checkbox"
						class="toggle toggle-accent"
						aria-label="Enable transcoding"
						bind:checked={transcodingEnabled}
					/>
				</label>

				{#if transcodingEnabled}
					<div class="grid gap-4 sm:grid-cols-2">
						<label class="flex flex-col">
							<span class="label-text mb-1">Default format</span>
							<select
								class="select select-bordered"
								aria-label="Transcode format"
								bind:value={transcodeFormat}
							>
								<option value="mp3">MP3</option>
								<option value="opus">Opus</option>
							</select>
						</label>
						<label class="flex flex-col">
							<span class="label-text mb-1">Max bitrate (kbps)</span>
							<input
								type="number"
								min="32"
								max="1411"
								class="input input-bordered"
								aria-label="Max bitrate kbps"
								bind:value={transcodeMaxKbps}
							/>
						</label>
					</div>
				{/if}

				<label class="flex flex-col">
					<span class="label-text mb-1">Similar-songs discovery</span>
					<select
						class="select select-bordered"
						aria-label="Discovery mode"
						bind:value={discoverMode}
					>
						<option value="local-only">Local only - same artist, no outbound calls</option>
						<option value="lazy-mb">Lazy MusicBrainz - fetch related artists once, cached</option>
						<option value="use-scrobble-targets"> Use Last.fm / ListenBrainz when linked </option>
					</select>
				</label>

				<div class="card-actions justify-end">
					<button class="btn btn-accent" onclick={handleSave} disabled={save.isPending}>
						{#if save.isPending}
							<span class="loading loading-spinner loading-sm"></span>
						{/if}
						Save
					</button>
				</div>
			</div>
		</div>
	{/if}

	<!-- oversight: every user's connected apps (metadata only, never the secret) -->
	<div class="card border border-base-300 bg-base-200">
		<div class="card-body gap-4">
			<div class="flex flex-wrap items-center justify-between gap-2">
				<h3 class="card-title text-base">
					<Users class="h-4 w-4" aria-hidden="true" />
					Connected app-passwords
				</h3>
				<span class="badge badge-ghost" aria-label="connected app-password count">
					{rosterCount} active
				</span>
			</div>
			<p class="text-sm text-base-content/60">
				Every app-password across all users. You can revoke any of them, but the secret itself is
				never shown to anyone after it's created.
			</p>

			{#if rosterQuery.isLoading}
				<div class="skeleton h-24 w-full rounded-box"></div>
			{:else if rosterQuery.isError}
				<div class="alert alert-error" role="alert">
					<TriangleAlert class="h-5 w-5" aria-hidden="true" />
					<span>Could not load the app-password list.</span>
					<button class="btn btn-sm" onclick={() => void rosterQuery.refetch()}>Try again</button>
				</div>
			{:else if rosterQuery.data && rosterQuery.data.items.length > 0}
				<div class="overflow-x-auto">
					<table class="table table-sm">
						<thead>
							<tr>
								<th>Owner</th>
								<th>Name</th>
								<th>Created</th>
								<th>Last used</th>
								<th>Last client</th>
								<th class="text-right">Action</th>
							</tr>
						</thead>
						<tbody>
							{#each rosterQuery.data.items as pw (pw.id)}
								<tr>
									<td>
										<span class="font-medium">{pw.owner_display_name}</span>
										<span class="block text-xs text-base-content/50">@{pw.owner_username}</span>
									</td>
									<td class="font-medium">{pw.name}</td>
									<td>{formatDate(pw.created_at)}</td>
									<td>{formatDate(pw.last_used_at)}</td>
									<td>{pw.last_client ?? '-'}</td>
									<td class="text-right">
										<button
											class="btn btn-ghost btn-xs text-error"
											aria-label={`Revoke ${pw.name} for ${pw.owner_username}`}
											onclick={() => (pendingRevoke = pw)}
										>
											<Trash2 class="h-4 w-4" aria-hidden="true" />
											Revoke
										</button>
									</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>
			{:else}
				<div
					class="rounded-box border border-dashed border-base-300 p-6 text-center text-base-content/60"
				>
					No app-passwords have been created yet.
				</div>
			{/if}
		</div>
	</div>

	<dialog
		bind:this={revokeDialog}
		class="modal"
		onclose={() => (pendingRevoke = null)}
		aria-label="Confirm revoke"
	>
		<div class="modal-box">
			<h3 class="text-lg font-bold">
				Revoke "{pendingRevoke?.name}" for {pendingRevoke?.owner_display_name}?
			</h3>
			<p class="py-2 text-sm text-base-content/70">
				This disconnects their app immediately; they can create a new one from their Profile.
			</p>
			<div class="modal-action">
				<button class="btn btn-ghost" onclick={() => revokeDialog?.close()}>Cancel</button>
				<button class="btn btn-error" onclick={confirmRevoke} disabled={adminRevoke.isPending}>
					{#if adminRevoke.isPending}
						<span class="loading loading-spinner loading-sm"></span>
					{/if}
					Revoke
				</button>
			</div>
		</div>
	</dialog>
</section>
