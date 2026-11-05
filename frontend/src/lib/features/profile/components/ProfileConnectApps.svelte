<script lang="ts">
	import { APP_PASSWORD_CAP } from '$lib/features/profile/queries/connect-apps.svelte';
	import { ChevronDown, Copy, Plus, Trash2, TriangleAlert, Wifi } from 'lucide-svelte';
	import JellyfinIcon from '$lib/media/icons/JellyfinIcon.svelte';

	import { getApiUrl } from '$lib/api/api-utils';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { toastStore } from '$lib/stores/toast';
	import {
		getAppPasswordsQuery,
		getConnectAppsSettingsQuery,
		getMyConnectAppsQuery
	} from '$lib/features/profile/queries/connect-apps.svelte';
	import {
		createAppPasswordMutation,
		createRevokeAppPasswordMutation,
		createSaveMyConnectAppsMutation
	} from '$lib/features/profile/queries/connect-apps.svelte';
	import type { AppPasswordView } from '$lib/features/profile/queries/connect-apps.svelte';
	import Button from '$lib/ui/Button.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import Field from '$lib/ui/Field.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { withBasePath } from '$lib/utils/basePath';

	const settingsQuery = getConnectAppsSettingsQuery();
	const passwordsQuery = getAppPasswordsQuery();
	const myChoicesQuery = getMyConnectAppsQuery();
	const saveChoices = createSaveMyConnectAppsMutation();
	const create = createAppPasswordMutation();
	const revoke = createRevokeAppPasswordMutation();

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

	// the list folds away once there is something in it
	let listOpen = $state<boolean | null>(null);
	const listShown = $derived(listOpen ?? activeCount === 0);
	const listId = $props.id();

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

	async function setIncludePublic(include: boolean) {
		try {
			await saveChoices.mutateAsync({ include_public_playlists: include });
		} catch {
			toastStore.show({ message: 'Could not save', type: 'error' });
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

{#if settingsQuery.isLoading || passwordsQuery.isLoading || myChoicesQuery.isLoading}
	<div class="space-y-3">
		<div class="skeleton h-14 w-full rounded-lg"></div>
		<div class="skeleton h-14 w-full rounded-lg"></div>
	</div>
{:else if settingsQuery.isError || passwordsQuery.isError || myChoicesQuery.isError}
	<div class="alert alert-error" role="alert">
		<TriangleAlert class="h-5 w-5" aria-hidden="true" />
		<span>Couldn't load Connect Apps.</span>
		<Button
			size="sm"
			onclick={() => {
				void settingsQuery.refetch();
				void passwordsQuery.refetch();
				void myChoicesQuery.refetch();
			}}
		>
			Try again
		</Button>
	</div>
{:else}
	<div class="space-y-3">
		{#if bothOff}
			<div
				class="flex items-start gap-3 rounded-lg border border-warning/40 bg-warning/8 p-3.5 text-sm"
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

		<div class="pf-box">
			<button
				type="button"
				class="ca-head"
				aria-expanded={listShown}
				aria-controls={listId}
				onclick={() => (listOpen = !listShown)}
			>
				<span class="ca-title">App-passwords</span>
				<span class="ca-count" aria-label="app-password count">{activeCount} / {cap}</span>
				<ChevronDown class="ca-chevron size-5" aria-hidden="true" />
			</button>

			{#if listShown}
				<div id={listId} class="border-t border-line px-4 pt-3 pb-4">
					<p class="mb-3 text-sm text-muted">
						An app-password lets one app stream your library while keeping your real account
						password private. Use a different one per device, and revoke it any time.
					</p>

					{#if passwordsQuery.data && passwordsQuery.data.length > 0}
						<div class="overflow-x-auto">
							<table class="ca-table table table-sm">
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
											<td class="font-semibold">{pw.name}</td>
											<td class="font-mono text-xs">{formatDate(pw.created_at)}</td>
											<td class="font-mono text-xs">{formatDate(pw.last_used_at)}</td>
											<td>{pw.last_client ?? '-'}</td>
											<td class="text-right">
												<Button
													variant="outline"
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
							class="rounded-lg border border-dashed border-edge p-5 text-center text-sm text-muted"
						>
							No app-passwords yet - create one to connect an app.
						</div>
					{/if}

					<div class="mt-4 flex flex-wrap items-end gap-2">
						<div class="min-w-48 flex-1">
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
							variant="primary"
							size="display-sm"
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
			{/if}
		</div>

		<div class="grid gap-3 lg:grid-cols-2">
			{#each [{ key: 'subsonic', label: 'OpenSubsonic', url: subsonicUrl, on: subsonicOn, clients: SUBSONIC_CLIENTS }, { key: 'jellyfin', label: 'Jellyfin', url: jellyfinUrl, on: jellyfinOn, clients: JELLYFIN_CLIENTS }] as proto (proto.key)}
				<div class="pf-box px-4 py-3.5">
					<h3 class="ca-proto">
						{#if proto.key === 'subsonic'}
							<Wifi class="size-6" aria-hidden="true" />
						{:else}
							<JellyfinIcon class="size-6" />
						{/if}
						<span class="flex-1">{proto.label}</span>
						{#if !proto.on}
							<StatusChip size="xs">not enabled yet</StatusChip>
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
						<span class="text-muted">Username:</span>
						<span class="font-mono">{username || 'your username'}</span>
					</p>
					<p class="mt-1 text-sm text-muted">
						Use your <strong class="text-base-content">app-password</strong> (above) as the password /
						API key.
					</p>
					<p class="mt-2 text-xs text-muted">Tested clients: {proto.clients}</p>
				</div>
			{/each}
		</div>

		<div class="pf-box px-4 py-3">
			<ToggleRow
				label="Show other people's public playlists"
				description="Your apps list only your own unless this is on."
				disabled={saveChoices.isPending}
				bind:checked={
					() => myChoicesQuery.data?.include_public_playlists ?? false,
					(include) => void setIncludePublic(include)
				}
			/>
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
	<p class="pb-2 text-sm text-muted">
		This is the only time "{revealedName}" will be shown. If you lose it, revoke it and create a new
		one.
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

<style>
	.ca-head {
		display: flex;
		width: 100%;
		align-items: center;
		gap: 1rem;
		padding: 0.875rem 1rem;
		border-radius: inherit;
		cursor: pointer;
	}
	.ca-head:hover {
		background-color: color-mix(in oklab, var(--color-base-content) 5%, transparent);
	}
	.ca-head:focus-visible {
		outline: 3px solid var(--dn-focus);
		outline-offset: 2px;
	}
	.ca-head[aria-expanded='true'] :global(.ca-chevron) {
		rotate: 180deg;
	}
	.ca-title {
		flex: 1;
		text-align: left;
		font-family: var(--font-mono);
		font-size: 1rem;
		font-weight: 600;
		letter-spacing: 0.22em;
		text-transform: uppercase;
	}
	@media (max-width: 639px) {
		.ca-title {
			font-size: 0.875rem;
			letter-spacing: 0.14em;
		}
	}
	.ca-count {
		font-family: var(--font-mono);
		font-size: 0.9375rem;
		font-weight: 600;
		letter-spacing: 0.12em;
	}
	.ca-table th {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		color: var(--color-base-content);
	}
	.ca-proto {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		margin-bottom: 0.75rem;
		font-family: var(--font-mono);
		font-size: 1rem;
		font-weight: 600;
		letter-spacing: 0.2em;
		text-transform: uppercase;
	}
</style>
