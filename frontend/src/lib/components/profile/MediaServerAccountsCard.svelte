<script lang="ts">
	import { LoaderCircle, ServerCog } from 'lucide-svelte';
	import { ApiError } from '$lib/api/client';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import { getConnectionsQuery } from '$lib/queries/connections/ConnectionsQuery.svelte';
	import {
		createConnectJellyfinMutation,
		createConnectNavidromeMutation,
		createDisconnectMutation,
		createPlexLinkPinMutation,
		createPlexLinkPollMutation
	} from '$lib/queries/connections/ConnectionsMutations.svelte';
	import type { ProfileServiceConnection } from '$lib/queries/profile/types';

	interface Props {
		services: ProfileServiceConnection[];
	}

	const { services }: Props = $props();

	const connectionsQuery = getConnectionsQuery();
	const connections = $derived(connectionsQuery.data?.connections ?? []);

	const navidromeEnabled = $derived(services.some((s) => s.name === 'Navidrome' && s.enabled));
	const jellyfinEnabled = $derived(services.some((s) => s.name === 'Jellyfin' && s.enabled));
	const plexEnabled = $derived(services.some((s) => s.name === 'Plex' && s.enabled));
	const anyEnabled = $derived(navidromeEnabled || jellyfinEnabled || plexEnabled);

	const connectNavidromeMutation = createConnectNavidromeMutation();
	const connectJellyfinMutation = createConnectJellyfinMutation();
	const plexPinMutation = createPlexLinkPinMutation();
	const plexPollMutation = createPlexLinkPollMutation();
	const disconnectMutation = createDisconnectMutation();

	interface CredentialFormState {
		open: boolean;
		username: string;
		password: string;
		error: string | null;
	}

	function emptyForm(): CredentialFormState {
		return { open: false, username: '', password: '', error: null };
	}

	let navidromeForm = $state(emptyForm());
	let jellyfinForm = $state(emptyForm());

	let plexPinId = $state<number | null>(null);
	let plexError = $state<string | null>(null);

	function errorMessage(e: unknown, fallback: string): string {
		return e instanceof ApiError ? e.message : fallback;
	}

	async function linkNavidrome() {
		navidromeForm.error = null;
		try {
			await connectNavidromeMutation.mutateAsync({
				username: navidromeForm.username.trim(),
				password: navidromeForm.password
			});
			navidromeForm = emptyForm();
		} catch (e) {
			navidromeForm.error = errorMessage(e, 'Could not sign in to Navidrome.');
		}
	}

	async function linkJellyfin() {
		jellyfinForm.error = null;
		try {
			await connectJellyfinMutation.mutateAsync({
				username: jellyfinForm.username.trim(),
				password: jellyfinForm.password
			});
			jellyfinForm = emptyForm();
		} catch (e) {
			jellyfinForm.error = errorMessage(e, 'Could not sign in to Jellyfin.');
		}
	}

	async function startPlexLink() {
		plexError = null;
		try {
			const pin = await plexPinMutation.mutateAsync();
			plexPinId = pin.pin_id;
			window.open(pin.auth_url, '_blank', 'popup=yes,noopener,noreferrer');
		} catch (e) {
			plexError = errorMessage(e, 'Could not start Plex sign-in.');
		}
	}

	function cancelPlexLink() {
		plexPinId = null;
		plexError = null;
	}

	// poll the pending pin until Plex reports the user approved it; the backend
	// stores the link server-side, so completion just needs the query refresh
	$effect(() => {
		const pinId = plexPinId;
		if (pinId === null) return;
		const interval = setInterval(async () => {
			try {
				const result = await plexPollMutation.mutateAsync(pinId);
				if (result.completed) {
					plexPinId = null;
				}
			} catch (e) {
				plexPinId = null;
				plexError = errorMessage(e, 'Plex sign-in failed.');
			}
		}, 3000);
		return () => clearInterval(interval);
	});

	async function disconnect(service: string) {
		await disconnectMutation.mutateAsync(service);
	}

	interface Row {
		service: string;
		label: string;
		icon: typeof NavidromeIcon;
		tint: string;
	}

	const rows = $derived(
		[
			navidromeEnabled && {
				service: 'navidrome',
				label: 'Navidrome',
				icon: NavidromeIcon,
				tint: 'bg-green-500/10 text-green-400 ring-green-500/20'
			},
			jellyfinEnabled && {
				service: 'jellyfin',
				label: 'Jellyfin',
				icon: JellyfinIcon,
				tint: 'bg-purple-500/10 text-purple-400 ring-purple-500/20'
			},
			plexEnabled && {
				service: 'plex',
				label: 'Plex',
				icon: PlexIcon,
				tint: 'bg-amber-500/10 text-amber-400 ring-amber-500/20'
			}
		].filter(Boolean) as Row[]
	);

	function linkedConnection(service: string) {
		return connections.find((c) => c.service === service);
	}
</script>

{#if anyEnabled}
	<section>
		<h2
			class="mb-4 flex items-center gap-2 text-sm font-semibold uppercase tracking-widest text-base-content/50"
		>
			<ServerCog class="h-4 w-4 text-accent" />
			Media Server Accounts
		</h2>

		<div
			class="glow-primary-soft space-y-3 rounded-2xl border border-base-300/50 bg-base-200/40 p-4 backdrop-blur-sm sm:p-5"
		>
			<p class="text-xs text-base-content/60">
				Link your own account on each server so plays and scrobbles count as you rather than the
				shared account.
			</p>

			{#if connectionsQuery.isPending}
				<div class="flex items-center justify-center py-10">
					<LoaderCircle class="h-5 w-5 animate-spin text-base-content/40" />
				</div>
			{:else}
				{#each rows as row (row.service)}
					{@const linked = linkedConnection(row.service)}
					{@const Icon = row.icon}
					<div>
						<div
							class="crate-card flex items-center justify-between gap-3 rounded-xl border border-base-300/40 bg-base-300/20 p-3"
						>
							<div class="flex min-w-0 items-center gap-3">
								<div
									class="flex h-10 w-10 items-center justify-center rounded-xl ring-1 {row.tint}"
								>
									<Icon class="h-[1.15rem] w-[1.15rem]" />
								</div>
								<div class="min-w-0">
									<div class="flex items-center gap-2">
										<span class="text-sm font-semibold">{row.label}</span>
										<span class="status {linked ? 'status-success' : 'status-error'} status-sm"
										></span>
									</div>
									{#if linked}
										<p class="truncate text-xs text-base-content/50">
											Plays count as @{linked.username || 'your account'}
										</p>
									{:else}
										<p class="text-xs text-base-content/30">Plays use the shared account</p>
									{/if}
								</div>
							</div>
							<div class="shrink-0">
								{#if linked}
									<button
										type="button"
										class="btn btn-ghost btn-xs rounded-full"
										onclick={() => disconnect(row.service)}
										disabled={disconnectMutation.isPending}
									>
										Disconnect
									</button>
								{:else if row.service === 'navidrome'}
									<button
										type="button"
										class="btn btn-primary btn-xs gap-1 rounded-full px-3 shadow-sm transition-transform hover:scale-[1.03]"
										onclick={() => (navidromeForm.open = !navidromeForm.open)}
									>
										Connect
									</button>
								{:else if row.service === 'jellyfin'}
									<button
										type="button"
										class="btn btn-primary btn-xs gap-1 rounded-full px-3 shadow-sm transition-transform hover:scale-[1.03]"
										onclick={() => (jellyfinForm.open = !jellyfinForm.open)}
									>
										Connect
									</button>
								{:else if plexPinId !== null}
									<div class="flex items-center gap-2">
										<button
											type="button"
											class="btn btn-ghost btn-xs rounded-full"
											onclick={cancelPlexLink}
										>
											Cancel
										</button>
										<span class="flex items-center gap-1 text-xs text-base-content/50">
											<LoaderCircle class="h-3.5 w-3.5 animate-spin" />
											Waiting for Plex…
										</span>
									</div>
								{:else}
									<button
										type="button"
										class="btn btn-primary btn-xs gap-1 rounded-full px-3 shadow-sm transition-transform hover:scale-[1.03]"
										onclick={startPlexLink}
										disabled={plexPinMutation.isPending}
									>
										Connect
									</button>
								{/if}
							</div>
						</div>

						{#if row.service === 'navidrome' && !linked && navidromeForm.open}
							<div
								class="mt-2 space-y-2 rounded-xl border border-base-300/40 bg-base-100/40 p-3 animate-fade-in-up"
							>
								<p class="text-xs text-base-content/60">
									Sign in with your own Navidrome username and password.
								</p>
								<input
									type="text"
									class="input input-sm input-soft w-full"
									placeholder="Navidrome username"
									bind:value={navidromeForm.username}
									autocomplete="off"
								/>
								<input
									type="password"
									class="input input-sm input-soft w-full"
									placeholder="Password"
									bind:value={navidromeForm.password}
									autocomplete="off"
								/>
								{#if navidromeForm.error}
									<p class="text-xs text-error">{navidromeForm.error}</p>
								{/if}
								<div class="flex justify-end gap-2">
									<button
										type="button"
										class="btn btn-ghost btn-xs rounded-full"
										onclick={() => (navidromeForm = emptyForm())}
									>
										Cancel
									</button>
									<button
										type="button"
										class="btn btn-primary btn-xs gap-1 rounded-full"
										onclick={linkNavidrome}
										disabled={connectNavidromeMutation.isPending ||
											!navidromeForm.username.trim() ||
											!navidromeForm.password}
									>
										{#if connectNavidromeMutation.isPending}
											<LoaderCircle class="h-3.5 w-3.5 animate-spin" />
										{/if}
										Link account
									</button>
								</div>
							</div>
						{/if}

						{#if row.service === 'jellyfin' && !linked && jellyfinForm.open}
							<div
								class="mt-2 space-y-2 rounded-xl border border-base-300/40 bg-base-100/40 p-3 animate-fade-in-up"
							>
								<p class="text-xs text-base-content/60">
									Sign in with your own Jellyfin username and password. The password is exchanged
									for an access token and never stored.
								</p>
								<input
									type="text"
									class="input input-sm input-soft w-full"
									placeholder="Jellyfin username"
									bind:value={jellyfinForm.username}
									autocomplete="off"
								/>
								<input
									type="password"
									class="input input-sm input-soft w-full"
									placeholder="Password"
									bind:value={jellyfinForm.password}
									autocomplete="off"
								/>
								{#if jellyfinForm.error}
									<p class="text-xs text-error">{jellyfinForm.error}</p>
								{/if}
								<div class="flex justify-end gap-2">
									<button
										type="button"
										class="btn btn-ghost btn-xs rounded-full"
										onclick={() => (jellyfinForm = emptyForm())}
									>
										Cancel
									</button>
									<button
										type="button"
										class="btn btn-primary btn-xs gap-1 rounded-full"
										onclick={linkJellyfin}
										disabled={connectJellyfinMutation.isPending ||
											!jellyfinForm.username.trim() ||
											!jellyfinForm.password}
									>
										{#if connectJellyfinMutation.isPending}
											<LoaderCircle class="h-3.5 w-3.5 animate-spin" />
										{/if}
										Link account
									</button>
								</div>
							</div>
						{/if}

						{#if row.service === 'plex' && !linked && plexError}
							<p class="mt-2 text-xs text-error">{plexError}</p>
						{/if}
					</div>
				{/each}
			{/if}
		</div>
	</section>
{/if}
