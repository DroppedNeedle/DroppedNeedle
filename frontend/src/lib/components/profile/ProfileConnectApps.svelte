<script lang="ts">
	import { APP_PASSWORD_CAP } from '$lib/queries/connect-apps/endpoints';
	import { Copy, Plus, Trash2, TriangleAlert, Waypoints } from 'lucide-svelte';

	import { getApiUrl } from '$lib/api/api-utils';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import {
		getAppPasswordsQuery,
		getConnectAppsSettingsQuery
	} from '$lib/queries/connect-apps/ConnectAppsQueries.svelte';
	import {
		createAppPassword,
		revokeAppPassword
	} from '$lib/queries/connect-apps/ConnectAppsMutations.svelte';
	import type { AppPasswordView } from '$lib/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import Field from '$lib/ui/Field.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	const settingsQuery = getConnectAppsSettingsQuery();
	const passwordsQuery = getAppPasswordsQuery();
	const create = createAppPassword();
	const revoke = revokeAppPassword();

	let newName = $state('');
	// The secret dialog is open while this is set; closing it clears the secret.
	let revealedSecret = $state<string | null>(null);
	let revealedName = $state('');
	let pendingRevoke = $state<AppPasswordView | null>(null);

	const isAdmin = $derived(authStore.isAdmin);
	const subsonicUrl = $derived(new URL(getApiUrl('/subsonic'), window.location.origin).toString());
	const jellyfinUrl = $derived(new URL(getApiUrl('/jellyfin'), window.location.origin).toString());
	const username = $derived(authStore.user?.username ?? '');

	const subsonicOn = $derived(settingsQuery.data?.subsonic_enabled ?? false);
	const jellyfinOn = $derived(settingsQuery.data?.jellyfin_enabled ?? false);
	const bothOff = $derived(!subsonicOn && !jellyfinOn);

	const cap = APP_PASSWORD_CAP;
	const activeCount = $derived(passwordsQuery.data?.length ?? 0);
	const atCap = $derived(activeCount >= cap);

	const SUGGESTED_NAMES = [
		'Symfonium (phone)',
		'Feishin (desktop)',
		'Amperfy (phone)',
		'Finamp (tablet)',
		'Jellify (phone)',
		'Manet (iPad)'
	];
	const SUBSONIC_CLIENTS = 'Symfonium, Feishin, Amperfy, DSub, Substreamer, play:Sub, Tempo';
	const JELLYFIN_CLIENTS = 'Finamp, Jellify, Manet Music (iOS), Symfonium (Jellyfin mode)';
	const CLIENT_NODES = ['Feishin', 'Manet', 'Arpeggi'];

	async function handleCreate() {
		const name = newName.trim();
		if (!name) {
			toastStore.show({ message: 'Give the app-password a name first', type: 'error' });
			return;
		}
		try {
			const result = await create.mutateAsync(name);
			revealedSecret = result.secret;
			revealedName = result.name;
			newName = '';
		} catch (err) {
			const status = (err as { status?: number })?.status;
			toastStore.show({
				message:
					status === 409 ? 'Limit reached - revoke one first' : 'Could not create app-password',
				type: 'error'
			});
		}
	}

	async function confirmRevoke() {
		const target = pendingRevoke;
		if (!target) return;
		try {
			await revoke.mutateAsync(target.id);
			toastStore.show({ message: `Revoked "${target.name}"`, type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not revoke app-password', type: 'error' });
		}
	}

	async function copy(text: string) {
		try {
			await navigator.clipboard.writeText(text);
			toastStore.show({ message: 'Copied', type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not copy', type: 'error' });
		}
	}

	function formatDate(unixSeconds: number | null | undefined): string {
		if (!unixSeconds) return 'Never';
		return new Date(unixSeconds * 1000).toLocaleDateString();
	}
</script>

<section>
	<h2
		class="mb-2 flex items-center gap-2 text-sm font-semibold uppercase tracking-widest text-base-content/50"
	>
		<Waypoints class="h-4 w-4 text-accent" />
		Connect Apps
	</h2>

	<!-- decorative motif: apps converging into DroppedNeedle -->
	<div class="cx-motif mb-5" aria-hidden="true">
		{#each CLIENT_NODES as node, i (node)}
			<span class="cx-node" style="--i: {i}">{node}</span>
		{/each}
		<span class="cx-arrow">→</span>
		<span class="cx-core"><span class="cx-core-dot"></span>DN</span>
	</div>

	{#if settingsQuery.isLoading || passwordsQuery.isLoading}
		<div class="space-y-4">
			<div class="skeleton h-40 w-full rounded-xl"></div>
			<div class="skeleton h-32 w-full rounded-xl"></div>
		</div>
	{:else if settingsQuery.isError || passwordsQuery.isError}
		<div class="alert alert-error" role="alert">
			<TriangleAlert class="h-5 w-5" aria-hidden="true" />
			<span>Couldn't load Connect Apps.</span>
			<Button
				size="sm"
				onclick={() => {
					void settingsQuery.refetch();
					void passwordsQuery.refetch();
				}}
			>
				Try again
			</Button>
		</div>
	{:else}
		<div class="space-y-4">
			{#if bothOff}
				<div
					class="flex items-start gap-3 rounded-xl border border-warning/30 bg-warning/5 p-4 text-sm"
					role="note"
				>
					<TriangleAlert class="mt-0.5 h-5 w-5 shrink-0 text-warning" aria-hidden="true" />
					{#if isAdmin}
						<p>
							Streaming isn't turned on yet. You can still create an app-password below, then
							<a href={withBasePath('/settings?tab=connect-apps')} class="link link-accent"
								>enable it in Settings</a
							>.
						</p>
					{:else}
						<p>
							Your admin hasn't turned on streaming yet. You can create an app-password now and it
							will work once they enable it.
						</p>
					{/if}
				</div>
			{/if}

			<!-- app-passwords -->
			<Card variant="bordered">
				<div>
					<div class="mb-2 flex flex-wrap items-center justify-between gap-2">
						<h3 class="font-semibold">App-passwords</h3>
						<span class="badge badge-ghost" aria-label="app-password count">
							{activeCount} / {cap}
						</span>
					</div>
					<p class="mb-4 text-sm text-base-content/60">
						An app-password lets one app stream your library while keeping your real account
						password private. Use a different one per device, and revoke it any time.
					</p>

					{#if passwordsQuery.data && passwordsQuery.data.length > 0}
						<div class="overflow-x-auto">
							<table class="table table-sm">
								<thead>
									<tr>
										<th>Name</th>
										<th>Created</th>
										<th>Last used</th>
										<th>Last client</th>
										<th class="text-right">Action</th>
									</tr>
								</thead>
								<tbody>
									{#each passwordsQuery.data as pw (pw.id)}
										<tr>
											<td class="font-medium">{pw.name}</td>
											<td>{formatDate(pw.created_at)}</td>
											<td>{formatDate(pw.last_used_at)}</td>
											<td>{pw.last_client ?? '-'}</td>
											<td class="text-right">
												<Button
													variant="danger-ghost"
													size="xs"
													icon={Trash2}
													aria-label={`Revoke ${pw.name}`}
													onclick={() => (pendingRevoke = pw)}
												>
													Revoke
												</Button>
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
							No app-passwords yet - create one to connect an app.
						</div>
					{/if}

					<div class="mt-4 flex flex-wrap items-end gap-2">
						<div class="flex-1">
							<Field label="New app-password name">
								{#snippet children(control)}
									<Input
										{...control}
										label="New app-password name"
										class="w-full"
										list="profile-connect-apps-suggested-names"
										placeholder="Symfonium (phone)"
										bind:value={newName}
									/>
								{/snippet}
							</Field>
							<datalist id="profile-connect-apps-suggested-names">
								{#each SUGGESTED_NAMES as suggestion (suggestion)}
									<option value={suggestion}></option>
								{/each}
							</datalist>
						</div>
						<Button
							variant="accent"
							icon={Plus}
							loading={create.isPending}
							disabled={atCap}
							title={atCap ? 'Revoke one first' : undefined}
							onclick={handleCreate}
						>
							Create
						</Button>
					</div>
				</div>
			</Card>

			<!-- how to connect -->
			<div class="grid gap-4 lg:grid-cols-2">
				{#each [{ key: 'subsonic', label: 'OpenSubsonic', url: subsonicUrl, on: subsonicOn, clients: SUBSONIC_CLIENTS }, { key: 'jellyfin', label: 'Jellyfin', url: jellyfinUrl, on: jellyfinOn, clients: JELLYFIN_CLIENTS }] as proto (proto.key)}
					<Card variant="bordered" class={proto.on ? '' : 'opacity-60'}>
						<div>
							<h3 class="mb-3 flex items-center gap-2 font-semibold">
								{proto.label}
								{#if !proto.on}
									<span class="badge badge-sm badge-ghost">not enabled yet</span>
								{/if}
							</h3>
							<Field label="Server URL">
								{#snippet children(control)}
									<div class="join w-full">
										<Input
											{...control}
											label={`${proto.label} server URL`}
											mono
											class="join-item flex-1"
											readonly
											value={proto.url}
										/>
										<IconButton
											label={`Copy ${proto.label} URL`}
											icon={Copy}
											variant="neutral"
											shape="square"
											class="join-item"
											onclick={() => copy(proto.url)}
										/>
									</div>
								{/snippet}
							</Field>
							<p class="mt-3 text-sm">
								<span class="text-base-content/60">Username:</span>
								<span class="font-mono">{username || 'your username'}</span>
							</p>
							<p class="mt-1 text-sm text-base-content/60">
								Use your <strong>app-password</strong> (above) as the password / API key.
							</p>
							<p class="mt-1 text-xs text-base-content/50">Tested clients: {proto.clients}</p>
						</div>
					</Card>
				{/each}
			</div>
		</div>
	{/if}

	<Modal
		bind:open={
			() => revealedSecret !== null,
			(open) => {
				if (!open) revealedSecret = null;
			}
		}
		title="Copy your app-password now"
	>
		<p class="pb-2 text-sm text-base-content/70">
			This is the only time "{revealedName}" will be shown. If you lose it, revoke it and create a
			new one.
		</p>
		{#if bothOff}
			<p class="pb-2 text-sm text-warning">
				Streaming isn't turned on yet, but this password will work once it's enabled.
			</p>
		{/if}
		<div class="join w-full">
			<Input
				label="App-password secret"
				mono
				class="join-item flex-1"
				readonly
				value={revealedSecret ?? ''}
			/>
			<Button
				variant="accent"
				icon={Copy}
				class="join-item"
				onclick={() => revealedSecret && copy(revealedSecret)}
			>
				Copy
			</Button>
		</div>
		{#snippet actions()}
			<Button onclick={() => (revealedSecret = null)}>Done</Button>
		{/snippet}
	</Modal>

	<ConfirmDialog
		bind:open={
			() => pendingRevoke !== null,
			(open) => {
				if (!open) pendingRevoke = null;
			}
		}
		title={`Revoke "${pendingRevoke?.name ?? ''}"?`}
		message="Any app or device using this app-password will be disconnected immediately and will need a new one."
		confirmLabel="Revoke"
		tone="danger"
		pending={revoke.isPending}
		onconfirm={confirmRevoke}
	/>
</section>

<style>
	.cx-motif {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.4rem 0.6rem;
		font-size: 0.72rem;
	}
	.cx-node {
		padding: 0.15rem 0.6rem;
		border-radius: 9999px;
		color: oklch(from var(--color-base-content) l c h / 0.7);
		background: oklch(from var(--color-base-content) l c h / 0.06);
		border: 1px solid oklch(from var(--color-accent) l c h / 0.25);
		animation: cx-node-pulse 3s ease-in-out infinite;
		animation-delay: calc(var(--i) * 0.25s);
	}
	.cx-arrow {
		color: oklch(from var(--color-accent) l c h / 0.7);
		font-weight: 700;
	}
	.cx-core {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		padding: 0.2rem 0.7rem;
		border-radius: 9999px;
		font-weight: 700;
		letter-spacing: 0.04em;
		color: var(--color-accent-content, var(--color-base-100));
		background: var(--color-accent);
		box-shadow: 0 0 18px oklch(from var(--color-accent) l c h / 0.3);
	}
	.cx-core-dot {
		width: 0.45rem;
		height: 0.45rem;
		border-radius: 9999px;
		background: var(--color-accent-content, var(--color-base-100));
		animation: cx-core-throb 2.4s ease-in-out infinite;
	}
	@keyframes cx-node-pulse {
		0%,
		100% {
			border-color: oklch(from var(--color-accent) l c h / 0.2);
		}
		50% {
			border-color: oklch(from var(--color-accent) l c h / 0.55);
		}
	}
	@keyframes cx-core-throb {
		0%,
		100% {
			transform: scale(1);
			opacity: 0.85;
		}
		50% {
			transform: scale(1.3);
			opacity: 1;
		}
	}
	@media (prefers-reduced-motion: reduce) {
		.cx-node,
		.cx-core-dot {
			animation: none;
		}
	}
</style>
