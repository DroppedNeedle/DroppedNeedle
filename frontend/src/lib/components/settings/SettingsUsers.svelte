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
		Gauge
	} from 'lucide-svelte';
	import UserQuotaEditor from '$lib/components/settings/UserQuotaEditor.svelte';
	import {
		getDownloadPolicyQuery,
		saveDownloadPolicy
	} from '$lib/queries/downloads/DownloadClientsQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { SvelteSet } from 'svelte/reactivity';

	interface UserRecord {
		id: string;
		display_name: string;
		role: 'admin' | 'trusted' | 'user';
		email: string | null;
		username: string | null;
		username_display: string | null;
		avatar_url: string | null;
		providers: string[];
	}

	const PAGE_SIZE = 20;

	let users = $state<UserRecord[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let savingRole = $state<string | null>(null);
	let roleError = $state<string | null>(null);
	let page = $state(1);
	let total = $state(0);

	let userToDelete = $state<UserRecord | null>(null);
	let deleteDialogEl: HTMLDialogElement | undefined = $state();
	let deleting = $state(false);
	let deleteError = $state<string | null>(null);

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
			const data = await api.get<{ users: UserRecord[]; total: number }>(
				`/api/v1/auth/admin/users?limit=${PAGE_SIZE}&offset=${offset}`
			);
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
			await api.patch(`/api/v1/auth/admin/users/${userId}/role`, { role });
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
			const user = await api.post<UserRecord>('/api/v1/auth/admin/users', {
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
		deleteError = null;
		userToDelete = user;
	}

	function closeDeleteDialog() {
		deleteDialogEl?.close();
		userToDelete = null;
		deleteError = null;
	}

	async function handleDeleteUser() {
		if (!userToDelete) return;
		deleting = true;
		deleteError = null;
		try {
			await api.delete(`/api/v1/auth/admin/users/${userToDelete.id}`);
			deleteDialogEl?.close();
			const wasLastOnPage = users.length === 1 && page > 1;
			userToDelete = null;
			await loadUsers(wasLastOnPage ? page - 1 : page);
		} catch (e: unknown) {
			const msg = (e as { message?: string })?.message;
			deleteError = msg ?? 'Could not delete user';
		} finally {
			deleting = false;
		}
	}

	$effect(() => {
		if (userToDelete) {
			deleteDialogEl?.showModal();
		}
	});

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

<div class="bg-base-200 rounded-box p-6 space-y-6">
	<div class="flex items-center justify-between">
		<div>
			<h2 class="text-lg font-semibold">User Management</h2>
			<p class="text-sm text-base-content/50 mt-0.5">Manage accounts and roles.</p>
		</div>
		<div class="flex gap-2">
			<button
				class="btn btn-ghost btn-sm btn-circle"
				onclick={() => void loadUsers(page)}
				aria-label="Refresh"
				disabled={loading}
			>
				<RefreshCw class="h-4 w-4 {loading ? 'animate-spin' : ''}" />
			</button>
			<button class="btn btn-outline btn-sm gap-1" onclick={() => (showImport = true)}>
				<Download class="h-4 w-4" />
				Import
			</button>
			<button
				class="btn btn-primary btn-sm gap-1"
				onclick={() => (showCreateForm = !showCreateForm)}
			>
				<Plus class="h-4 w-4" />
				New User
			</button>
		</div>
	</div>

	<SettingsImportUsers bind:open={showImport} onImported={() => void loadUsers(page)} />

	{#if showCreateForm}
		<div class="bg-base-300/50 rounded-box p-4 border border-base-300">
			<h3 class="text-sm font-semibold mb-4 text-base-content/70 uppercase tracking-wider">
				Create User
			</h3>
			<form
				onsubmit={(e) => {
					e.preventDefault();
					void handleCreateUser();
				}}
				class="grid grid-cols-1 sm:grid-cols-2 gap-3"
			>
				<fieldset class="fieldset">
					<legend class="fieldset-legend">Display Name</legend>
					<input
						type="text"
						class="input input-bordered w-full input-sm"
						bind:value={newName}
						required
						placeholder="Jane Smith"
					/>
				</fieldset>
				<fieldset class="fieldset">
					<legend class="fieldset-legend">Username</legend>
					<input
						type="text"
						class="input input-bordered w-full input-sm"
						bind:value={newUsername}
						required
						autocomplete="off"
						placeholder="jane.smith"
					/>
				</fieldset>
				<fieldset class="fieldset">
					<legend class="fieldset-legend">Email (optional)</legend>
					<input
						type="email"
						class="input input-bordered w-full input-sm"
						bind:value={newEmail}
						placeholder="jane@example.com"
					/>
				</fieldset>
				<fieldset class="fieldset">
					<legend class="fieldset-legend">Password</legend>
					<label class="input input-bordered flex items-center gap-2 w-full input-sm">
						{#if showNewPassword}
							<input
								type="text"
								class="grow"
								bind:value={newPassword}
								required
								placeholder="Min. 12 chars"
							/>
						{:else}
							<input
								type="password"
								class="grow"
								bind:value={newPassword}
								required
								placeholder="Min. 12 chars"
							/>
						{/if}
						<button
							type="button"
							onclick={() => (showNewPassword = !showNewPassword)}
							class="opacity-50 hover:opacity-100"
							aria-label="Toggle"
						>
							{#if showNewPassword}
								<EyeOff class="h-3.5 w-3.5" />
							{:else}
								<Eye class="h-3.5 w-3.5" />
							{/if}
						</button>
					</label>
				</fieldset>
				<fieldset class="fieldset">
					<legend class="fieldset-legend">Role</legend>
					<select class="select select-bordered w-full select-sm" bind:value={newRole}>
						<option value="user">User</option>
						<option value="trusted">Trusted</option>
						<option value="admin">Admin</option>
					</select>
				</fieldset>
				{#if createError}
					<div class="sm:col-span-2 alert alert-error py-2 text-sm">{createError}</div>
				{/if}
				<div class="sm:col-span-2 flex gap-2 justify-end">
					<button
						type="button"
						class="btn btn-ghost btn-sm"
						onclick={() => (showCreateForm = false)}
					>
						Cancel
					</button>
					<button type="submit" class="btn btn-primary btn-sm" disabled={creating}>
						{#if creating}<span class="loading loading-spinner loading-xs"></span>{/if}
						Create
					</button>
				</div>
			</form>
		</div>
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
				<div
					class="flex items-center gap-3 p-3 bg-base-300/30 rounded-box hover:bg-base-300/50 transition-colors"
				>
					<div
						class="w-9 h-9 rounded-full bg-primary/10 flex items-center justify-center shrink-0 overflow-hidden"
					>
						{#if user.avatar_url}
							<img
								src={user.avatar_url}
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
										<PlexIcon class="h-3.5 w-3.5" style="color: rgb(var(--brand-plex))" />
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
								class="select select-bordered select-xs"
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
						<button
							class="btn btn-ghost btn-sm btn-circle {quotaOpen.has(user.id)
								? 'text-primary'
								: 'text-base-content/50 hover:text-primary'}"
							onclick={() => toggleQuota(user.id)}
							aria-expanded={quotaOpen.has(user.id)}
							aria-label="Request and storage quota"
							title="Request and storage quota"
						>
							<Gauge class="h-4 w-4" />
						</button>
						<button
							class="btn btn-ghost btn-sm btn-circle text-error/70 hover:text-error hover:bg-error/10"
							onclick={() => confirmDelete(user)}
							disabled={user.id === authStore.user?.id}
							aria-label="Delete user"
							title={user.id === authStore.user?.id
								? 'You cannot delete your own account'
								: `Delete ${user.display_name}`}
						>
							<Trash2 class="h-4 w-4" />
						</button>
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
			<button
				class="btn btn-sm btn-outline"
				disabled={page === 1 || loading}
				onclick={() => void loadUsers(page - 1)}>Previous</button
			>
			<span class="text-sm text-base-content/60">
				Showing {rangeStart}-{rangeEnd} of {total} users (page {page} of {totalPages})
			</span>
			<button
				class="btn btn-sm btn-outline"
				disabled={page >= totalPages || loading}
				onclick={() => void loadUsers(page + 1)}>Next</button
			>
		</div>
	{/if}

	<div class="pt-2 border-t border-base-300 space-y-2">
		<h3 class="text-xs font-semibold text-base-content/50 uppercase tracking-wider">
			Quota defaults
		</h3>
		<p class="text-xs text-base-content/60">
			Limits for every plain user without an override (admin and trusted are exempt). 0 = unlimited.
			Requests use a rolling window; storage counts each user's own downloads.
		</p>
		<div class="flex flex-wrap items-end gap-2">
			<label class="form-control">
				<span class="label-text text-xs">Requests</span>
				<input
					type="number"
					min="0"
					class="input input-bordered input-sm w-24"
					bind:value={defRequestCount}
				/>
			</label>
			<label class="form-control">
				<span class="label-text text-xs">Window (days)</span>
				<input
					type="number"
					min="1"
					class="input input-bordered input-sm w-24"
					bind:value={defRequestDays}
				/>
			</label>
			<label class="form-control">
				<span class="label-text text-xs">Storage (GB)</span>
				<input
					type="number"
					min="0"
					class="input input-bordered input-sm w-28"
					bind:value={defStorageGb}
				/>
			</label>
			<button
				class="btn btn-primary btn-sm"
				onclick={handleSaveQuotaDefaults}
				disabled={savePolicy.isPending || !defaultsSeeded}
			>
				Save defaults
			</button>
		</div>
	</div>

	<div class="pt-2 border-t border-base-300 space-y-2">
		<h3 class="text-xs font-semibold text-base-content/50 uppercase tracking-wider">Role guide</h3>
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

<dialog bind:this={deleteDialogEl} class="modal" onclose={closeDeleteDialog}>
	<div class="modal-box max-w-md">
		<h3 class="text-lg font-bold">Delete User</h3>
		<p class="py-4 text-base-content/70">
			Delete <span class="font-semibold text-base-content">{userToDelete?.display_name}</span>? This
			permanently removes their account, login methods, and sessions. This cannot be undone.
		</p>

		{#if deleteError}
			<div class="alert alert-error py-2 text-sm">{deleteError}</div>
		{/if}

		<div class="modal-action">
			<button class="btn btn-ghost" onclick={closeDeleteDialog} disabled={deleting}>
				Cancel
			</button>
			<button class="btn btn-error" onclick={() => void handleDeleteUser()} disabled={deleting}>
				{#if deleting}
					<span class="loading loading-spinner loading-sm"></span>
					Deleting...
				{:else}
					Delete
				{/if}
			</button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button>close</button>
	</form>
</dialog>
