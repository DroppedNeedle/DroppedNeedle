<script lang="ts">
	import { LoaderCircle } from 'lucide-svelte';
	import { ApiError } from '$lib/api/errors';
	import JellyfinIcon from '$lib/media/icons/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/media/icons/NavidromeIcon.svelte';
	import PlexIcon from '$lib/media/icons/PlexIcon.svelte';
	import { getConnectionsQuery } from '$lib/features/profile/queries/ConnectionsQuery.svelte';
	import {
		createConnectJellyfinMutation,
		createConnectNavidromeMutation,
		createDisconnectMutation,
		createPlexLinkPinMutation,
		createPlexLinkPollMutation
	} from '$lib/features/profile/queries/ConnectionsMutations.svelte';
	import type { ProfileServiceConnection } from '$lib/features/profile/queries/profile.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Input from '$lib/ui/Input.svelte';

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
				tint: 'text-navidrome'
			},
			jellyfinEnabled && {
				service: 'jellyfin',
				label: 'Jellyfin',
				icon: JellyfinIcon,
				tint: 'text-jellyfin'
			},
			plexEnabled && {
				service: 'plex',
				label: 'Plex',
				icon: PlexIcon,
				tint: 'text-plex'
			}
		].filter(Boolean) as Row[]
	);

	function linkedConnection(service: string) {
		return connections.find((c) => c.service === service);
	}
</script>

{#snippet credentialForm(
	form: CredentialFormState,
	label: string,
	note: string,
	pending: boolean,
	link: () => Promise<void>,
	cancel: () => void
)}
	<div class="pf-box mt-2 animate-fade-in-up p-3">
		<div class="space-y-2">
			<p class="text-xs text-muted">{note}</p>
			<Input
				label="{label} username"
				size="sm"
				class="w-full"
				placeholder="{label} username"
				autocomplete="off"
				bind:value={form.username}
			/>
			<Input
				type="password"
				label="{label} password"
				size="sm"
				class="w-full"
				placeholder="Password"
				autocomplete="off"
				bind:value={form.password}
			/>
			{#if form.error}
				<p class="text-xs text-error">{form.error}</p>
			{/if}
			<div class="flex justify-end gap-2">
				<Button variant="ghost" size="sm" onclick={cancel}>Cancel</Button>
				<Button
					variant="primary"
					size="sm"
					loading={pending}
					disabled={!form.username.trim() || !form.password}
					onclick={link}
				>
					Link account
				</Button>
			</div>
		</div>
	</div>
{/snippet}

{#if anyEnabled}
	<p class="mb-3 text-sm text-muted">
		Link your own account on each server so plays and scrobbles count as you rather than the shared
		account.
	</p>

	{#if connectionsQuery.isPending}
		<div class="flex items-center justify-center py-10">
			<LoaderCircle class="h-5 w-5 animate-spin motion-keep text-muted" />
		</div>
	{:else}
		<div class="space-y-2.5">
			{#each rows as row (row.service)}
				{@const linked = linkedConnection(row.service)}
				{@const Icon = row.icon}
				<div>
					<div class="pf-box flex flex-wrap items-center gap-3 px-4 py-3">
						<span class="ma-icon {row.tint}" aria-hidden="true">
							<Icon class="size-5" />
						</span>
						<div class="min-w-0 flex-1">
							<p class="flex items-center gap-2 font-semibold">
								{row.label}
								<span
									class="status status-sm {linked ? 'status-success' : 'status-error'}"
									aria-hidden="true"
								></span>
							</p>
							{#if linked}
								<p class="pf-meta truncate">
									Plays count as @{linked.username || 'your account'}
								</p>
							{:else}
								<p class="pf-meta">Plays use the shared account</p>
							{/if}
						</div>
						<div class="shrink-0">
							{#if linked}
								<Button
									variant="outline"
									size="display-sm"
									onclick={() => disconnect(row.service)}
									disabled={disconnectMutation.isPending}
								>
									Disconnect
								</Button>
							{:else if row.service === 'navidrome'}
								<Button
									variant="primary"
									size="display-sm"
									onclick={() => (navidromeForm.open = !navidromeForm.open)}
								>
									Connect
								</Button>
							{:else if row.service === 'jellyfin'}
								<Button
									variant="primary"
									size="display-sm"
									onclick={() => (jellyfinForm.open = !jellyfinForm.open)}
								>
									Connect
								</Button>
							{:else if plexPinId !== null}
								<div class="flex items-center gap-2">
									<Button variant="ghost" size="sm" onclick={cancelPlexLink}>Cancel</Button>
									<span class="flex items-center gap-1 text-xs text-muted">
										<LoaderCircle class="h-3.5 w-3.5 animate-spin motion-keep" />
										Waiting for Plex…
									</span>
								</div>
							{:else}
								<Button
									variant="primary"
									size="display-sm"
									onclick={startPlexLink}
									disabled={plexPinMutation.isPending}
								>
									Connect
								</Button>
							{/if}
						</div>
					</div>

					{#if row.service === 'navidrome' && !linked && navidromeForm.open}
						{@render credentialForm(
							navidromeForm,
							'Navidrome',
							'Sign in with your own Navidrome username and password.',
							connectNavidromeMutation.isPending,
							linkNavidrome,
							() => (navidromeForm = emptyForm())
						)}
					{/if}

					{#if row.service === 'jellyfin' && !linked && jellyfinForm.open}
						{@render credentialForm(
							jellyfinForm,
							'Jellyfin',
							'Sign in with your own Jellyfin username and password. The password is exchanged for an access token and never stored.',
							connectJellyfinMutation.isPending,
							linkJellyfin,
							() => (jellyfinForm = emptyForm())
						)}
					{/if}

					{#if row.service === 'plex' && !linked && plexError}
						<p class="mt-2 text-sm text-error">{plexError}</p>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
{/if}

<style>
	.ma-icon {
		display: grid;
		place-items: center;
		width: 2.75rem;
		height: 2.75rem;
		border-radius: 0.5rem;
		background-color: var(--color-base-300);
	}
</style>
