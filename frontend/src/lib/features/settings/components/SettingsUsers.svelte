<script lang="ts">
	import {
		createAdminUserMutation,
		createDeleteUserMutation,
		createRevokeUserSessionsMutation,
		createSetUserRoleMutation,
		getAdminUsersQuery,
		type AdminUser
	} from '$lib/features/settings/queries/AdminUsersQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import JellyfinIcon from '$lib/media/icons/JellyfinIcon.svelte';
	import PlexIcon from '$lib/media/icons/PlexIcon.svelte';
	import SettingsImportUsers from '$lib/features/settings/components/SettingsImportUsers.svelte';
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
		Clock3,
		LogOut
	} from 'lucide-svelte';
	import UserQuotaEditor from '$lib/features/settings/components/UserQuotaEditor.svelte';
	import {
		getDownloadPolicyQuery,
		createSaveDownloadPolicyMutation
	} from '$lib/features/downloads/queries/DownloadClientsQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { SvelteSet } from 'svelte/reactivity';
	import { createPasswordRecoveryCodeMutation } from '$lib/features/auth/queries/AuthMutations.svelte';
	import { getApiUrl } from '$lib/api/api-utils';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import Field from '$lib/ui/Field.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import SettingsPaneHeader from './SettingsPaneHeader.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import TextField from '$lib/ui/TextField.svelte';

	type UserRecord = AdminUser;

	const PAGE_SIZE = 20;

	let savingRole = $state<string | null>(null);
	let roleError = $state<string | null>(null);
	let page = $state(1);

	const usersQuery = getAdminUsersQuery(() => ({
		limit: PAGE_SIZE,
		offset: (page - 1) * PAGE_SIZE
	}));
	const setUserRole = createSetUserRoleMutation();
	const createUser = createAdminUserMutation();
	const deleteUser = createDeleteUserMutation();
	const revokeSessions = createRevokeUserSessionsMutation();
	const users = $derived(usersQuery.data?.users ?? []);
	const total = $derived(usersQuery.data?.total ?? 0);
	const loading = $derived(usersQuery.isFetching);
	const error = $derived(usersQuery.isError ? "Couldn't load users" : null);

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
	const savePolicy = createSaveDownloadPolicyMutation();

	interface QuotaDefaults {
		default_request_quota_count: number | null;
		default_request_quota_days: number | null;
		default_storage_quota_gb: number | null;
	}

	function quotaDefaultsOf(policy: QuotaDefaults): QuotaDefaults {
		return {
			default_request_quota_count: policy.default_request_quota_count,
			default_request_quota_days: policy.default_request_quota_days,
			default_storage_quota_gb: policy.default_storage_quota_gb
		};
	}

	// The quota defaults are three fields of the download policy.
	const quotaDefaults = createSettingsSection<QuotaDefaults>(
		{
			get data() {
				const policy = policyQuery.data;
				return policy ? quotaDefaultsOf(policy) : undefined;
			},
			get isPending() {
				return policyQuery.isPending;
			},
			get isError() {
				return policyQuery.isError;
			},
			get error() {
				return policyQuery.error;
			},
			refetch: () => policyQuery.refetch()
		},
		{
			mutateAsync: async (form) => {
				const policy = policyQuery.data;
				if (!policy) throw new Error('The download policy has not loaded.');
				// A cleared number input binds null: off, or the window as stored.
				const saved = await savePolicy.mutateAsync({
					...policy,
					default_request_quota_count: form.default_request_quota_count ?? 0,
					default_request_quota_days:
						form.default_request_quota_days ?? policy.default_request_quota_days,
					default_storage_quota_gb: form.default_storage_quota_gb ?? 0
				});
				return quotaDefaultsOf(saved);
			},
			get isPending() {
				return savePolicy.isPending;
			}
		},
		{ savedMessage: 'Quota defaults saved.' }
	);

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

	function loadUsers(targetPage = page) {
		if (targetPage !== page) page = targetPage;
		else void usersQuery.refetch();
	}

	async function setRole(userId: string, role: 'admin' | 'trusted' | 'user') {
		savingRole = userId;
		roleError = null;
		try {
			await setUserRole.mutateAsync({ userId, role });
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
			const user = await createUser.mutateAsync({
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
			// The list is oldest first, so the new account is on the last page.
			page = Math.max(1, Math.ceil(total / PAGE_SIZE));
		} catch (e: unknown) {
			const msg = (e as { message?: string })?.message;
			createError = msg ?? 'Could not create user';
		} finally {
			creating = false;
		}
	}

	let userToSignOut = $state<UserRecord | null>(null);
	let signOutOpen = $state(false);

	function confirmSignOut(user: UserRecord) {
		userToSignOut = user;
		signOutOpen = true;
	}

	async function handleSignOut() {
		const user = userToSignOut;
		if (!user) return;
		await revokeSessions.mutateAsync(user.id);
		toastStore.show({ message: `Signed out ${user.display_name} everywhere`, type: 'success' });
		userToSignOut = null;
	}

	function confirmDelete(user: UserRecord) {
		userToDelete = user;
		deleteOpen = true;
	}

	// Throws on failure, so the dialog stays open and shows why.
	async function handleDeleteUser() {
		if (!userToDelete) return;
		const wasLastOnPage = users.length === 1 && page > 1;
		try {
			await deleteUser.mutateAsync(userToDelete.id);
		} catch (e: unknown) {
			const msg = (e as { message?: string })?.message;
			throw new Error(msg ?? 'Could not delete user', { cause: e });
		}
		userToDelete = null;
		if (wasLastOnPage) page -= 1;
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

	function roleTone(role: string): 'action' | 'info' | 'neutral' {
		if (role === 'admin') return 'action';
		if (role === 'trusted') return 'info';
		return 'neutral';
	}
</script>

<div class="space-y-5">
	<SettingsPaneHeader title="User Management" description="Manage accounts and roles.">
		{#snippet actions()}
			<IconButton icon={RefreshCw} label="Refresh" {loading} onclick={() => void loadUsers(page)} />
			<Button variant="paper" size="display-sm" icon={Download} onclick={() => (showImport = true)}>
				Import
			</Button>
			<Button
				variant="primary"
				size="display-sm"
				icon={Plus}
				aria-expanded={showCreateForm}
				onclick={() => (showCreateForm = !showCreateForm)}
			>
				New User
			</Button>
		{/snippet}
	</SettingsPaneHeader>

	<Card padding="md">
		<div class="space-y-5">
			<SettingsImportUsers bind:open={showImport} onImported={() => void loadUsers(page)} />

			{#if showCreateForm}
				<Card variant="bordered" padding="md">
					<h3 class="dn-set-kicker mb-1">Create User</h3>
					<form
						onsubmit={(e) => {
							e.preventDefault();
							void handleCreateUser();
						}}
						class="grid grid-cols-1 sm:grid-cols-2 gap-3"
					>
						<TextField
							label="Display Name"
							bind:value={newName}
							required
							placeholder="Jane Smith"
						/>
						<TextField
							label="Username"
							bind:value={newUsername}
							required
							placeholder="jane.smith"
						/>
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
						<div class="flex items-center gap-3 px-2 py-3 animate-pulse">
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
				<div class="divide-y divide-line">
					{#each users as user (user.id)}
						{@const RoleIcon = roleIcon(user.role)}
						{@const canRecover = user.providers.includes('local')}
						{@const isSelf = user.id === authStore.user?.id}
						<div class="users-row">
							<div class="users-row__avatar">
								{#if user.avatar_url}
									<img
										src={getApiUrl(user.avatar_url)}
										alt={user.display_name}
										class="h-full w-full object-cover"
									/>
								{:else}
									<UserRound class="h-5 w-5" aria-hidden="true" />
								{/if}
							</div>
							<div class="flex-1 min-w-32">
								<p class="font-bold truncate">{user.display_name}</p>
								{#if user.username_display ?? user.username}
									<p class="font-mono text-xs text-muted truncate">
										@{user.username_display ?? user.username}
									</p>
								{/if}
								{#if user.email}
									<p class="text-xs text-muted truncate">{user.email}</p>
								{/if}
							</div>
							{#if user.providers.length > 0}
								<div class="flex items-center gap-1.5 shrink-0">
									{#each user.providers as provider (provider)}
										<div class="tooltip" data-tip={providerLabel[provider] ?? provider}>
											{#if provider === 'jellyfin'}
												<JellyfinIcon class="h-4 w-4 text-info" />
											{:else if provider === 'plex'}
												<PlexIcon class="h-4 w-4 text-plex" />
											{:else if provider === 'oidc'}
												<KeyRound class="h-4 w-4 text-muted" />
											{:else}
												<Mail class="h-4 w-4 text-muted" />
											{/if}
										</div>
									{/each}
								</div>
							{/if}
							<div
								class="flex w-full flex-wrap items-center justify-end gap-2 sm:w-auto sm:shrink-0"
							>
								<span class="hidden sm:contents">
									<StatusChip tone={roleTone(user.role)} icon={RoleIcon}>
										{roleLabel[user.role]}
									</StatusChip>
								</span>
								{#if savingRole === user.id}
									<span class="loading loading-spinner loading-xs"></span>
								{:else}
									<select
										class="select select-sm w-28 shrink-0"
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
								<span
									title={canRecover ? undefined : 'This account does not have a local password'}
								>
									<IconButton
										size="sm"
										icon={KeyRound}
										label={`Create recovery code for ${user.display_name}`}
										disabled={!canRecover}
										onclick={() => openRecoveryDialog(user)}
									/>
								</span>
								<span title={isSelf ? 'Sign yourself out from your profile' : undefined}>
									<IconButton
										size="sm"
										icon={LogOut}
										label="Sign out everywhere"
										disabled={isSelf}
										onclick={() => confirmSignOut(user)}
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
					<span class="text-sm text-muted">
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

			<div class="space-y-2 border-t border-line pt-5">
				<h3 class="dn-set-kicker">Quota defaults</h3>
				<p class="text-sm text-muted">
					Limits for every plain user without an override (admin and trusted are exempt). 0 =
					unlimited. Requests use a rolling window; storage counts each user's own downloads.
				</p>
				<QueryBoundary query={quotaDefaults} what="the quota defaults">
					{#snippet children(draft)}
						<div class="flex flex-wrap items-start gap-2">
							<Field
								label="Requests"
								error={quotaDefaults.fieldError('default_request_quota_count', 'Requests')}
							>
								{#snippet children(control)}
									<Input
										{...control}
										label="Requests"
										type="number"
										size="sm"
										min="0"
										class="w-24"
										bind:value={draft.default_request_quota_count}
									/>
								{/snippet}
							</Field>
							<Field
								label="Window (days)"
								error={quotaDefaults.fieldError('default_request_quota_days', 'Window')}
							>
								{#snippet children(control)}
									<Input
										{...control}
										label="Window (days)"
										type="number"
										size="sm"
										min="1"
										class="w-24"
										bind:value={draft.default_request_quota_days}
									/>
								{/snippet}
							</Field>
							<Field
								label="Storage (GB)"
								error={quotaDefaults.fieldError('default_storage_quota_gb', 'Storage')}
							>
								{#snippet children(control)}
									<Input
										{...control}
										label="Storage (GB)"
										type="number"
										size="sm"
										min="0"
										class="w-28"
										bind:value={draft.default_storage_quota_gb}
									/>
								{/snippet}
							</Field>
						</div>
						<SaveBar {...quotaDefaults.bar} saveLabel="Save defaults" />
					{/snippet}
				</QueryBoundary>
			</div>

			<div class="space-y-3 border-t border-line pt-5">
				<h3 class="dn-set-kicker">Role guide</h3>
				<div class="grid gap-2.5 text-sm text-muted">
					<div class="flex gap-2">
						<ShieldCheck class="h-4 w-4 text-accent shrink-0 mt-0.5" />
						<span
							><strong class="text-base-content">Admin</strong>, full access, approves requests,
							manages users.</span
						>
					</div>
					<div class="flex gap-2">
						<UserCheck class="h-4 w-4 text-info shrink-0 mt-0.5" />
						<span
							><strong class="text-base-content">Trusted</strong>, requests auto-approved, no admin
							functions.</span
						>
					</div>
					<div class="flex gap-2">
						<UserX class="h-4 w-4 text-muted shrink-0 mt-0.5" />
						<span
							><strong class="text-base-content">User</strong>, requests need admin approval before
							downloading.</span
						>
					</div>
				</div>
			</div>
		</div>
	</Card>
</div>

<ConfirmDialog
	bind:open={deleteOpen}
	title="Delete User"
	confirmLabel="Delete"
	tone="danger"
	onconfirm={handleDeleteUser}
	oncancel={() => (userToDelete = null)}
>
	<p class="text-muted">
		Delete <span class="font-semibold text-base-content">{userToDelete?.display_name}</span>? This
		permanently removes their account, login methods, sessions, and playlists, public ones included.
		This cannot be undone.
	</p>
</ConfirmDialog>

<ConfirmDialog
	bind:open={signOutOpen}
	title="Sign out everywhere?"
	confirmLabel="Sign out"
	tone="danger"
	onconfirm={handleSignOut}
	oncancel={() => (userToSignOut = null)}
>
	<p class="text-muted">
		Signs <span class="font-semibold text-base-content">{userToSignOut?.display_name}</span> out of every
		browser and device. Their app passwords keep working.
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
			<p class="text-sm leading-relaxed text-muted">
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
			<div class="flex items-center gap-2 text-xs text-muted">
				<Clock3 class="h-4 w-4 text-accent" aria-hidden="true" />
				Expires at {recoveryExpiryLabel(recoveryExpiresAt)}. Creating another code will replace this
				one.
			</div>
		</div>
	{:else}
		<p class="text-sm leading-relaxed text-muted">
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

<style>
	.users-row {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.75rem;
		padding: 0.875rem 0.5rem;
	}
	.users-row__avatar {
		display: flex;
		flex: none;
		align-items: center;
		justify-content: center;
		width: 2.5rem;
		height: 2.5rem;
		overflow: hidden;
		border: 2px solid var(--dn-title);
		border-radius: 9999px;
		color: var(--dn-title);
	}
</style>
