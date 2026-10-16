<script lang="ts">
	import { onMount } from 'svelte';
	import { api } from '$lib/api/client';
	import { authStore } from '$lib/stores/authStore.svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import SettingsImportUsers from '$lib/components/settings/SettingsImportUsers.svelte';
	import {
		UserRound,
		ShieldCheck,
		UserCheck,
		UserX,
		Plus,
		Download,
		Eye,
		EyeOff,
		RefreshCw,
		Trash2,
		Mail,
		KeyRound,
		Gauge,
		Copy,
		Check,
		Clock3
	} from 'lucide-svelte';
	import UserQuotaEditor from '$lib/components/settings/UserQuotaEditor.svelte';
	import {
		getDownloadPolicyQuery,
		saveDownloadPolicy
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { SvelteSet } from 'svelte/reactivity';
	import { createPasswordRecoveryCodeMutation } from '$lib/queries/auth/AuthMutations.svelte';
	import { getApiUrl } from '$lib/api/api-utils';
	import type { components } from '$lib/api/v3/openapi';
	import { AUTH_ENDPOINTS } from '$lib/queries/auth/endpoints';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import Field from '$lib/ui/Field.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import TextField from '$lib/ui/TextField.svelte';

	type UserRecord = components['schemas']['UserResponse'];

	const PAGE_SIZE = 20;

	let users = $state<UserRecord[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let savingRole = $state<string | null>(null);
	let roleError = $state<string | null>(null);
	let page = $state(1);
	let total = $state(0);

	let userToDelete = $state<UserRecord | null>(null);
	let deleteOpen = $state(false);

	const createRecoveryCode = createPasswordRecoveryCodeMutation();
	let recoveryUser = $state<UserRecord | null>(null);
	let recoveryOpen = $state(false);
	let recoveryCode = $state<string | null>(null);
	let recoveryExpiresAt = $state<number | null>(null);
	let recoveryError = $state<string | null>(null);
	let recoveryCopied = $state(false);

	const totalPages = $derived(Math.max(1, Math.ceil(total / PAGE_SIZE)));
	const rangeStart = $derived(total === 0 ? 0 : (page - 1) * PAGE_SIZE + 1);
	const rangeEnd = $derived(Math.min(page * PAGE_SIZE, total));

	let showImport = $state(false);

	// Per-user quotas (CollectionManagement Feature C): rows with the editor open,
	// plus the global defaults every user without an override inherits.
	const quotaOpen = new SvelteSet<string>();
	function toggleQuota(userId: string) {
		if (quotaOpen.has(userId)) quotaOpen.delete(userId);
		else quotaOpen.add(userId);
	}

	const policyQuery = getDownloadPolicyQuery();
	const savePolicy = saveDownloadPolicy();
	let defRequestCount = $state<number | null>(0);
	let defRequestDays = $state<number | null>(7);
	let defStorageGb = $state<number | null>(0);
	let defaultsSeeded = $state(false);

	$effect(() => {
		const p = policyQuery.data;
		if (p && !defaultsSeeded) {
			defRequestCount = p.default_request_quota_count;
			defRequestDays = p.default_request_quota_days;
			defStorageGb = p.default_storage_quota_gb;
			defaultsSeeded = true;
		}
	});

	async function handleSaveQuotaDefaults() {
		const p = policyQuery.data;
		if (!p) return;
		try {
			// a cleared number input binds null; fall back to off/current values
			await savePolicy.mutateAsync({
				...p,
				default_request_quota_count: defRequestCount ?? 0,
				default_request_quota_days: defRequestDays ?? p.default_request_quota_days,
				default_storage_quota_gb: defStorageGb ?? 0
			});
			toastStore.show({ message: 'Quota defaults saved', type: 'success' });
		} catch {
			toastStore.show({ message: 'Could not save quota defaults', type: 'error' });
		}
	}

	let showCreateForm = $state(false);
	let newName = $state('');
	let newUsername = $state('');
	let newEmail = $state('');
	let newPassword = $state('');
	let newRole = $state<'admin' | 'trusted' | 'user'>('user');
	let showNewPassword = $state(false);
	let creating = $state(false);
	let createError = $state<string | null>(null);
	let createSuccess = $state<string | null>(null);

	async function loadUsers(targetPage = page) {
		loading = true;
		error = null;
		try {
			const offset = (targetPage - 1) * PAGE_SIZE;
			const data = await api.v3.GET(AUTH_ENDPOINTS.adminUsers(PAGE_SIZE, offset));
			users = data.users;
			total = data.total;
			page = targetPage;
		} catch {
			error = "Couldn't load users";
		} finally {
			loading = false;
		}
	}

	async function setRole(userId: string, role: 'admin' | 'trusted' | 'user') {
		savingRole = userId;
		roleError = null;
		try {
			await api.v3.PUT(AUTH_ENDPOINTS.adminUserRole(userId), { role });
			users = users.map((u) => (u.id === userId ? { ...u, role } : u));
		} catch (e: unknown) {
			roleError = (e as { message?: string })?.message ?? 'Could not update role';
		} finally {
			savingRole = null;
		}
	}

	async function handleCreateUser() {
		createError = null;
		createSuccess = null;
		if (newPassword.length < 12) {
			createError = 'Password must be at least 12 characters';
			return;
		}
		creating = true;
		try {
			const user = await api.v3.POST(AUTH_ENDPOINTS.adminCreateUser, {
				display_name: newName,
				username: newUsername,
				email: newEmail || undefined,
				password: newPassword,
				role: newRole
			});
			createSuccess = `Created ${user.display_name}`;
			newName = '';
			newUsername = '';
			newEmail = '';
			newPassword = '';
			newRole = 'user';
			showCreateForm = false;
			total += 1;
			const lastPage = Math.ceil(total / PAGE_SIZE);
			if (lastPage === page && users.length < PAGE_SIZE) {
				// Admin-created users always get a local (email/password) login
				users = [...users, { ...user, providers: ['local'] }];
			} else {
				// New user landed on a different page (sorted by created_at ASC)
				await loadUsers(lastPage);
			}
		} catch (e: unknown) {
			const msg = (e as { message?: string })?.message;
			createError = msg ?? 'Could not create user';
		} finally {
			creating = false;
		}
	}

	function confirmDelete(user: UserRecord) {
		userToDelete = user;
		deleteOpen = true;
	}

	// Throws on failure, so the dialog stays open and shows why.
	async function handleDeleteUser() {
		if (!userToDelete) return;
		try {
			await api.v3.DELETE(AUTH_ENDPOINTS.adminUser(userToDelete.id));
		} catch (e: unknown) {
			const msg = (e as { message?: string })?.message;
			throw new Error(msg ?? 'Could not delete user', { cause: e });
		}
		const wasLastOnPage = users.length === 1 && page > 1;
		userToDelete = null;
		await loadUsers(wasLastOnPage ? page - 1 : page);
	}

	function openRecoveryDialog(user: UserRecord) {
		recoveryUser = user;
		recoveryCode = null;
		recoveryExpiresAt = null;
		recoveryError = null;
		recoveryCopied = false;
		createRecoveryCode.reset();
		recoveryOpen = true;
	}

	function closeRecoveryDialog() {
		recoveryOpen = false;
		recoveryUser = null;
		recoveryCode = null;
		recoveryExpiresAt = null;
		recoveryError = null;
		recoveryCopied = false;
		createRecoveryCode.reset();
	}

	async function handleCreateRecoveryCode() {
		if (!recoveryUser) return;
		const requestedUserId = recoveryUser.id;
		recoveryError = null;
		try {
			const result = await createRecoveryCode.mutateAsync(requestedUserId);
			if (recoveryUser?.id !== requestedUserId) return;
			recoveryCode = result.recovery_code;
			recoveryExpiresAt = result.expires_at;
			toastStore.show({ message: 'Recovery code created', type: 'success' });
		} catch (cause: unknown) {
			if (recoveryUser?.id !== requestedUserId) return;
			const message = cause instanceof Error ? cause.message : 'Could not create recovery code';
			recoveryError = message;
			toastStore.show({ message, type: 'error' });
		}
	}

	async function copyRecoveryCode() {
		if (!recoveryCode) return;
		try {
			await navigator.clipboard.writeText(recoveryCode);
			recoveryCopied = true;
			setTimeout(() => (recoveryCopied = false), 2000);
		} catch {
			toastStore.show({ message: 'Could not copy recovery code', type: 'error' });
		}
	}

	function recoveryExpiryLabel(value: number | null): string {
		if (value == null) return '';
		// v3 reports expiry as unix seconds.
		const expires = new Date(value * 1000);
		if (Number.isNaN(expires.getTime())) return '15 minutes after creation';
		return expires.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
	}

	const roleOptions: { value: 'admin' | 'trusted' | 'user'; label: string }[] = [
		{ value: 'user', label: 'User' },
		{ value: 'trusted', label: 'Trusted' },
		{ value: 'admin', label: 'Admin' }
	];

	const roleLabel: Record<string, string> = {
		admin: 'Admin',
		trusted: 'Trusted',
		user: 'User'
	};

	const providerLabel: Record<string, string> = {
		local: 'Email',
		jellyfin: 'Jellyfin',
		plex: 'Plex',
		oidc: 'SSO'
	};

	function roleIcon(role: string) {
		if (role === 'admin') return ShieldCheck;
		if (role === 'trusted') return UserCheck;
		return UserX;
	}

	function roleBadgeClass(role: string) {
		if (role === 'admin') return 'badge-accent';
		if (role === 'trusted') return 'badge-info';
		return 'badge-ghost';
	}

	onMount(() => {
		void loadUsers();
	});
</script>

<Card>
	<div class="space-y-6">
		<div class="flex items-center justify-between">
			<div>
				<h2 class="text-lg font-semibold">User Management</h2>
				<p class="text-sm text-base-content/50 mt-0.5">Manage accounts and roles.</p>
			</div>
			<div class="flex gap-2">
				<IconButton
					size="sm"
					icon={RefreshCw}
					label="Refresh"
					{loading}
					onclick={() => void loadUsers(page)}
				/>
				<Button variant="outline" size="sm" icon={Download} onclick={() => (showImport = true)}>
					Import
				</Button>
				<Button
					variant="primary"
					size="sm"
					icon={Plus}
					onclick={() => (showCreateForm = !showCreateForm)}
				>
					New User
				</Button>
			</div>
		</div>

		<SettingsImportUsers bind:open={showImport} onImported={() => void loadUsers(page)} />

		{#if showCreateForm}
			<Card variant="bordered" padding="md">
				<h3 class="text-sm font-semibold mb-2 text-base-content/70 uppercase tracking-wider">
					Create User
				</h3>
				<form
					onsubmit={(e) => {
						e.preventDefault();
						void handleCreateUser();
					}}
					class="grid grid-cols-1 sm:grid-cols-2 gap-3"
				>
					<TextField label="Display Name" bind:value={newName} required placeholder="Jane Smith" />
					<TextField label="Username" bind:value={newUsername} required placeholder="jane.smith" />
					<TextField
						label="Email (optional)"
						type="email"
						bind:value={newEmail}
						placeholder="jane@example.com"
					/>
					<Field label="Password">
						{#snippet children(control)}
							<div class="join w-full">
								<Input
									{...control}
									label="Password"
									type={showNewPassword ? 'text' : 'password'}
									class="join-item w-full"
									autocomplete="new-password"
									required
									placeholder="Min. 12 chars"
									bind:value={newPassword}
								/>
								<IconButton
									variant="neutral"
									shape="square"
									class="join-item"
									icon={showNewPassword ? EyeOff : Eye}
									label={showNewPassword ? 'Hide password' : 'Show password'}
									onclick={() => (showNewPassword = !showNewPassword)}
								/>
							</div>
						{/snippet}
					</Field>
					<SelectField label="Role" options={roleOptions} bind:value={newRole} />
					{#if createError}
						<div class="sm:col-span-2 alert alert-error py-2 text-sm">{createError}</div>
					{/if}
					<div class="sm:col-span-2 flex gap-2 justify-end">
						<Button variant="ghost" size="sm" onclick={() => (showCreateForm = false)}>
							Cancel
						</Button>
						<Button type="submit" variant="primary" size="sm" loading={creating}>Create</Button>
					</div>
				</form>
			</Card>
		{/if}

		{#if createSuccess}
			<div class="alert alert-success py-2 text-sm">{createSuccess}</div>
		{/if}

		{#if roleError}
			<div class="alert alert-error py-2 text-sm">{roleError}</div>
		{/if}

		{#if error}
			<div class="alert alert-error py-2 text-sm">{error}</div>
		{/if}

		{#if loading && users.length === 0}
			<div class="space-y-2">
				{#each Array(3) as _, i (`user-skel-${i}`)}
					<div class="flex items-center gap-3 p-3 bg-base-300/40 rounded-box animate-pulse">
						<div class="w-9 h-9 rounded-full bg-base-300"></div>
						<div class="flex-1">
							<div class="h-3.5 bg-base-300 rounded w-32 mb-1.5"></div>
							<div class="h-3 bg-base-300 rounded w-48"></div>
						</div>
						<div class="h-6 bg-base-300 rounded-full w-16"></div>
					</div>
				{/each}
			</div>
		{:else}
			<div class="space-y-1.5">
				{#each users as user (user.id)}
					{@const RoleIcon = roleIcon(user.role)}
					{@const canRecover = user.providers.includes('local')}
					{@const isSelf = user.id === authStore.user?.id}
					<div
						class="flex items-center gap-3 p-3 bg-base-300/30 rounded-box hover:bg-base-300/50 transition-colors"
					>
						<div
							class="w-9 h-9 rounded-full bg-primary/10 flex items-center justify-center shrink-0 overflow-hidden"
						>
							{#if user.avatar_url}
								<img
									src={getApiUrl(user.avatar_url)}
									alt={user.display_name}
									class="h-full w-full object-cover"
								/>
							{:else}
								<UserRound class="h-5 w-5 text-primary/60" />
							{/if}
						</div>
						<div class="flex-1 min-w-0">
							<p class="text-sm font-medium truncate">{user.display_name}</p>
							{#if user.username_display ?? user.username}
								<p class="text-xs text-base-content/40 truncate">
									@{user.username_display ?? user.username}
								</p>
							{/if}
							{#if user.email}
								<p class="text-xs text-base-content/50 truncate">{user.email}</p>
							{/if}
						</div>
						{#if user.providers.length > 0}
							<div class="flex items-center gap-1.5 shrink-0">
								{#each user.providers as provider (provider)}
									<div class="tooltip" data-tip={providerLabel[provider] ?? provider}>
										{#if provider === 'jellyfin'}
											<JellyfinIcon class="h-3.5 w-3.5 text-info" />
										{:else if provider === 'plex'}
											<PlexIcon class="h-3.5 w-3.5 text-plex" />
										{:else if provider === 'oidc'}
											<KeyRound class="h-3.5 w-3.5 text-base-content/40" />
										{:else}
											<Mail class="h-3.5 w-3.5 text-base-content/40" />
										{/if}
									</div>
								{/each}
							</div>
						{/if}
						<div class="flex items-center gap-2 shrink-0">
							<span class="badge {roleBadgeClass(user.role)} badge-sm gap-1">
								<RoleIcon class="h-3 w-3" />
								{roleLabel[user.role]}
							</span>
							{#if savingRole === user.id}
								<span class="loading loading-spinner loading-xs"></span>
							{:else}
								<select
									class="select select-sm"
									value={user.role}
									onchange={(e) =>
										void setRole(
											user.id,
											(e.target as HTMLSelectElement).value as 'admin' | 'trusted' | 'user'
										)}
									aria-label="Change role"
								>
									<option value="user">User</option>
									<option value="trusted">Trusted</option>
									<option value="admin">Admin</option>
								</select>
							{/if}
							<IconButton
								size="sm"
								icon={Gauge}
								label="Request and storage quota"
								active={quotaOpen.has(user.id)}
								aria-expanded={quotaOpen.has(user.id)}
								onclick={() => toggleQuota(user.id)}
							/>
							<span title={canRecover ? undefined : 'This account does not have a local password'}>
								<IconButton
									size="sm"
									icon={KeyRound}
									label={`Create recovery code for ${user.display_name}`}
									disabled={!canRecover}
									onclick={() => openRecoveryDialog(user)}
								/>
							</span>
							<span title={isSelf ? 'You cannot delete your own account' : undefined}>
								<IconButton
									size="sm"
									variant="danger-ghost"
									icon={Trash2}
									label="Delete user"
									disabled={isSelf}
									onclick={() => confirmDelete(user)}
								/>
							</span>
						</div>
					</div>
					{#if quotaOpen.has(user.id)}
						<UserQuotaEditor userId={user.id} displayName={user.display_name} />
					{/if}
				{/each}
			</div>
		{/if}

		{#if total > PAGE_SIZE}
			<div class="flex items-center justify-between">
				<Button
					variant="outline"
					size="sm"
					disabled={page === 1 || loading}
					onclick={() => void loadUsers(page - 1)}>Previous</Button
				>
				<span class="text-sm text-base-content/60">
					Showing {rangeStart}-{rangeEnd} of {total} users (page {page} of {totalPages})
				</span>
				<Button
					variant="outline"
					size="sm"
					disabled={page >= totalPages || loading}
					onclick={() => void loadUsers(page + 1)}>Next</Button
				>
			</div>
		{/if}

		<div class="pt-2 border-t border-base-300 space-y-2">
			<h3 class="text-xs font-semibold text-base-content/50 uppercase tracking-wider">
				Quota defaults
			</h3>
			<p class="text-xs text-base-content/60">
				Limits for every plain user without an override (admin and trusted are exempt). 0 =
				unlimited. Requests use a rolling window; storage counts each user's own downloads.
			</p>
			<div class="flex flex-wrap items-end gap-2">
				<Field label="Requests">
					{#snippet children(control)}
						<Input
							{...control}
							label="Requests"
							type="number"
							size="sm"
							min="0"
							class="w-24"
							bind:value={defRequestCount}
						/>
					{/snippet}
				</Field>
				<Field label="Window (days)">
					{#snippet children(control)}
						<Input
							{...control}
							label="Window (days)"
							type="number"
							size="sm"
							min="1"
							class="w-24"
							bind:value={defRequestDays}
						/>
					{/snippet}
				</Field>
				<Field label="Storage (GB)">
					{#snippet children(control)}
						<Input
							{...control}
							label="Storage (GB)"
							type="number"
							size="sm"
							min="0"
							class="w-28"
							bind:value={defStorageGb}
						/>
					{/snippet}
				</Field>
				<Button
					variant="primary"
					size="sm"
					class="mb-1"
					onclick={handleSaveQuotaDefaults}
					loading={savePolicy.isPending}
					disabled={!defaultsSeeded}
				>
					Save defaults
				</Button>
			</div>
		</div>

		<div class="pt-2 border-t border-base-300 space-y-2">
			<h3 class="text-xs font-semibold text-base-content/50 uppercase tracking-wider">
				Role guide
			</h3>
			<div class="grid gap-2 text-xs text-base-content/60">
				<div class="flex gap-2">
					<ShieldCheck class="h-4 w-4 text-accent shrink-0 mt-0.5" />
					<span
						><strong class="text-base-content/80">Admin</strong>, full access, approves requests,
						manages users.</span
					>
				</div>
				<div class="flex gap-2">
					<UserCheck class="h-4 w-4 text-info shrink-0 mt-0.5" />
					<span
						><strong class="text-base-content/80">Trusted</strong>, requests auto-approved, no admin
						functions.</span
					>
				</div>
				<div class="flex gap-2">
					<UserX class="h-4 w-4 text-base-content/40 shrink-0 mt-0.5" />
					<span
						><strong class="text-base-content/80">User</strong>, requests need admin approval before
						downloading.</span
					>
				</div>
			</div>
		</div>
	</div>
</Card>

<ConfirmDialog
	bind:open={deleteOpen}
	title="Delete User"
	confirmLabel="Delete"
	tone="danger"
	onconfirm={handleDeleteUser}
	oncancel={() => (userToDelete = null)}
>
	<p class="text-base-content/70">
		Delete <span class="font-semibold text-base-content">{userToDelete?.display_name}</span>? This
		permanently removes their account, login methods, and sessions. This cannot be undone.
	</p>
</ConfirmDialog>

<Modal
	bind:open={recoveryOpen}
	title={recoveryCode ? 'Recovery code ready' : 'Create recovery code'}
	description={recoveryCode ? `For ${recoveryUser?.display_name}` : undefined}
	dismissible={!createRecoveryCode.isPending}
	onclose={closeRecoveryDialog}
>
	{#if recoveryCode}
		<div class="space-y-4">
			<p class="text-sm leading-relaxed text-base-content/65">
				Share this code privately. It can be used once and will not be shown again after you close
				this window.
			</p>
			<Card variant="bordered" padding="md" class="text-center">
				<code class="block break-all font-mono text-lg font-semibold tracking-wider text-primary">
					{recoveryCode}
				</code>
				<Button
					variant="outline"
					size="sm"
					class="mx-auto mt-2"
					icon={recoveryCopied ? Check : Copy}
					onclick={() => void copyRecoveryCode()}
				>
					{recoveryCopied ? 'Copied' : 'Copy code'}
				</Button>
			</Card>
			<div class="flex items-center gap-2 text-xs text-base-content/50">
				<Clock3 class="h-4 w-4 text-accent" aria-hidden="true" />
				Expires at {recoveryExpiryLabel(recoveryExpiresAt)}. Creating another code will replace this
				one.
			</div>
		</div>
	{:else}
		<p class="text-sm leading-relaxed text-base-content/65">
			Create a 15-minute, single-use code for
			<span class="font-semibold text-base-content">{recoveryUser?.display_name}</span>? Their
			current password remains valid until the code is used.
		</p>

		{#if recoveryError}
			<div class="alert alert-error mt-4 py-2 text-sm" role="alert">{recoveryError}</div>
		{/if}
	{/if}

	{#snippet actions()}
		{#if recoveryCode}
			<Button variant="primary" onclick={closeRecoveryDialog}>Done</Button>
		{:else}
			<Button variant="ghost" onclick={closeRecoveryDialog} disabled={createRecoveryCode.isPending}>
				Cancel
			</Button>
			<Button
				variant="primary"
				loading={createRecoveryCode.isPending}
				onclick={() => void handleCreateRecoveryCode()}
			>
				Create code
			</Button>
		{/if}
	{/snippet}
</Modal>
