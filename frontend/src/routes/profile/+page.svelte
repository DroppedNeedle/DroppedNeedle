<script lang="ts">
	import { ApiError } from '$lib/api/client';
	import {
		UserRound,
		Pencil,
		Check,
		X,
		Radio,
		Music,
		HardDrive,
		Database,
		Disc3,
		Users,
		Settings,
		Camera,
		ExternalLink,
		ImagePlus,
		CircleAlert,
		RefreshCw,
		LogOut,
		ShieldCheck,
		UserCheck,
		UserCog,
		AtSign,
		Mail,
		KeyRound
	} from 'lucide-svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { logout } from '$lib/utils/logout';
	import { getProfileQuery } from '$lib/queries/profile/ProfileQuery.svelte';
	import {
		createUpdateDisplayNameMutation,
		createUpdateUsernameMutation,
		createUpdateEmailMutation,
		createChangePasswordMutation,
		createSetPasswordMutation,
		createUploadAvatarMutation
	} from '$lib/queries/profile/ProfileMutations.svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import NavidromeIcon from '$lib/components/NavidromeIcon.svelte';
	import type { ProfileServiceConnection } from '$lib/queries/profile/types';
	import ScrobblingDiscoveryCard from '$lib/components/profile/ScrobblingDiscoveryCard.svelte';
	import SpotifyConnectionCard from '$lib/components/profile/SpotifyConnectionCard.svelte';
	import { page } from '$app/state';
	import { browser } from '$app/environment';
	import { onMount } from 'svelte';
	import { toastStore } from '$lib/stores/toast';
	import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
	import { ConnectionsQueryKeyFactory } from '$lib/queries/connections/ConnectionsQueryKeyFactory';

	const userId = authStore.user?.id ?? '';
	const profileQuery = getProfileQuery(userId);
	const profile = $derived(profileQuery.data);
	const providers = $derived(profile?.providers ?? authStore.user?.providers ?? []);
	const hasLocalPassword = $derived(providers.includes('local'));

	// lastfm + listenbrainz are per-user (managed in the scrobbling card), so drop from the read-only grid
	const HIDDEN_SERVICES = new Set(['ListenBrainz', 'Last.fm']);
	const visibleServices = $derived(
		(profile?.services ?? []).filter((s) => !HIDDEN_SERVICES.has(s.name))
	);

	const displayNameMutation = createUpdateDisplayNameMutation(userId);
	const usernameMutation = createUpdateUsernameMutation(userId);
	const emailMutation = createUpdateEmailMutation(userId);
	const changePasswordMutation = createChangePasswordMutation(userId);
	const setPasswordMutation = createSetPasswordMutation(userId);
	const avatarMutation = createUploadAvatarMutation(userId);

	let editingName = $state(false);
	let nameInput = $state('');
	let nameError = $state<string | null>(null);

	let editingUsername = $state(false);
	let usernameInput = $state('');
	let usernameError = $state<string | null>(null);

	let editingEmail = $state(false);
	let emailInput = $state('');
	let emailError = $state<string | null>(null);

	let showPasswordForm = $state(false);
	let currentPassword = $state('');
	let newPassword = $state('');
	let passwordError = $state<string | null>(null);
	let passwordDone = $state(false);

	let showAvatarModal = $state(false);
	let avatarPreview: string | null = $state(null);
	let avatarFile: File | null = $state(null);
	let draggingOver = $state(false);
	let fileInput: HTMLInputElement | undefined = $state();

	onMount(async () => {
		const spotify = page.url.searchParams.get('spotify');
		if (spotify === 'connected') {
			toastStore.show({ message: 'Spotify connected successfully', type: 'success' });
			await invalidateQueriesWithPersister({
				queryKey: ConnectionsQueryKeyFactory.list(authStore.user?.id)
			});
			if (browser) history.replaceState({}, '', '/profile');
		} else if (spotify === 'error') {
			toastStore.show({ message: 'Spotify connection failed. Please try again.', type: 'error' });
			if (browser) history.replaceState({}, '', '/profile');
		}
	});

	function errMessage(e: unknown): string {
		return e instanceof ApiError ? e.message : 'Could not reach the server';
	}

	function startEditName() {
		nameInput = profile?.display_name ?? '';
		nameError = null;
		editingName = true;
	}

	async function saveName() {
		nameError = null;
		try {
			await displayNameMutation.mutateAsync({ display_name: nameInput });
			editingName = false;
		} catch (e) {
			nameError = errMessage(e);
		}
	}

	function startEditUsername() {
		usernameInput = profile?.username_display ?? profile?.username ?? '';
		usernameError = null;
		editingUsername = true;
	}

	async function saveUsername() {
		usernameError = null;
		try {
			await usernameMutation.mutateAsync({ username: usernameInput });
			editingUsername = false;
		} catch (e) {
			usernameError = errMessage(e);
		}
	}

	function startEditEmail() {
		emailInput = profile?.email ?? '';
		emailError = null;
		editingEmail = true;
	}

	async function saveEmail() {
		emailError = null;
		try {
			await emailMutation.mutateAsync({ email: emailInput.trim() || null });
			editingEmail = false;
		} catch (e) {
			emailError = errMessage(e);
		}
	}

	function togglePasswordForm() {
		showPasswordForm = !showPasswordForm;
		currentPassword = '';
		newPassword = '';
		passwordError = null;
		passwordDone = false;
	}

	async function submitPassword() {
		passwordError = null;
		try {
			if (hasLocalPassword) {
				await changePasswordMutation.mutateAsync({
					current_password: currentPassword,
					new_password: newPassword
				});
			} else {
				await setPasswordMutation.mutateAsync({ new_password: newPassword });
			}
			currentPassword = '';
			newPassword = '';
			showPasswordForm = false;
			passwordDone = true;
		} catch (e) {
			passwordError = errMessage(e);
		}
	}

	const passwordPending = $derived(
		changePasswordMutation.isPending || setPasswordMutation.isPending
	);

	function handleAvatarFile(file: File) {
		if (!file.type.startsWith('image/')) return;
		avatarFile = file;
		if (avatarPreview) URL.revokeObjectURL(avatarPreview);
		avatarPreview = URL.createObjectURL(file);
	}

	function handleDrop(e: DragEvent) {
		e.preventDefault();
		draggingOver = false;
		const file = e.dataTransfer?.files[0];
		if (file) handleAvatarFile(file);
	}

	function handleDragOver(e: DragEvent) {
		e.preventDefault();
		draggingOver = true;
	}

	function handleFileSelect(e: Event) {
		const input = e.target as HTMLInputElement;
		const file = input.files?.[0];
		if (file) handleAvatarFile(file);
	}

	async function uploadAvatar() {
		if (!avatarFile) return;
		try {
			await avatarMutation.mutateAsync(avatarFile);
			closeAvatarModal();
		} catch {
			// keep the modal open to retry; failure shows via disabled/loading state
		}
	}

	function closeAvatarModal() {
		showAvatarModal = false;
		avatarFile = null;
		if (avatarPreview) {
			URL.revokeObjectURL(avatarPreview);
			avatarPreview = null;
		}
	}

	function handleNameKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') void saveName();
		if (e.key === 'Escape') editingName = false;
	}

	function handleUsernameKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') void saveUsername();
		if (e.key === 'Escape') editingUsername = false;
	}

	function handleEmailKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') void saveEmail();
		if (e.key === 'Escape') editingEmail = false;
	}

	function getServiceIcon(name: string) {
		if (name === 'Jellyfin') return JellyfinIcon;
		if (name === 'Navidrome') return NavidromeIcon;
		if (name === 'ListenBrainz') return Music;
		if (name === 'Last.fm') return Radio;
		return Database;
	}

	function getServiceColor(name: string): string {
		if (name === 'Jellyfin') return 'text-purple-400';
		if (name === 'Navidrome') return 'text-green-400';
		if (name === 'ListenBrainz') return 'text-orange-400';
		if (name === 'Last.fm') return 'text-red-400';
		return 'text-base-content';
	}

	function getServiceBorderColor(name: string): string {
		if (name === 'Jellyfin') return 'border-purple-500/30';
		if (name === 'Navidrome') return 'border-green-500/30';
		if (name === 'ListenBrainz') return 'border-orange-500/30';
		if (name === 'Last.fm') return 'border-red-500/30';
		return 'border-base-300';
	}

	function getServiceProfileUrl(service: ProfileServiceConnection): string | null {
		if (!service.enabled || !service.username) return null;
		if (service.name === 'Last.fm')
			return `https://www.last.fm/user/${encodeURIComponent(service.username)}`;
		if (service.name === 'ListenBrainz')
			return `https://listenbrainz.org/user/${encodeURIComponent(service.username)}`;
		if (service.name === 'Jellyfin' && service.url) return service.url;
		if (service.name === 'Navidrome' && service.url) return service.url;
		return null;
	}

	function getSourceIcon(source: string) {
		if (source === 'Jellyfin') return JellyfinIcon;
		if (source === 'Navidrome') return NavidromeIcon;
		if (source === 'Local Files') return HardDrive;
		return Database;
	}

	function getSourceColor(source: string): string {
		if (source === 'Jellyfin') return 'text-purple-400';
		if (source === 'Navidrome') return 'text-green-400';
		if (source === 'Local Files') return 'text-teal-400';
		return 'text-base-content';
	}

	function formatNumber(n: number): string {
		if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
		return n.toString();
	}

	const roleLabel: Record<string, string> = {
		admin: 'Admin',
		trusted: 'Trusted',
		user: 'User'
	};
</script>

<svelte:head>
	<title>Profile - DroppedNeedle</title>
</svelte:head>

<div class="min-h-screen">
	<div class="relative overflow-hidden">
		<div class="absolute inset-0 bg-linear-to-br from-primary/20 via-accent/10 to-base-100"></div>
		<div class="absolute inset-0 bg-linear-to-t from-base-100 via-base-100/60 to-transparent"></div>

		<div class="relative px-4 pt-10 pb-6 sm:px-6 lg:px-8">
			<div class="mx-auto max-w-4xl">
				{#if profileQuery.isPending}
					<div class="flex flex-col items-center gap-6">
						<div class="skeleton h-32 w-32 rounded-full sm:h-40 sm:w-40"></div>
						<div class="flex flex-col items-center gap-2">
							<div class="skeleton h-8 w-48"></div>
							<div class="skeleton h-4 w-32"></div>
						</div>
					</div>
				{:else if profile}
					<div class="flex flex-col items-center gap-6">
						<button
							onclick={() => (showAvatarModal = true)}
							class="group relative h-32 w-32 shrink-0 cursor-pointer overflow-hidden rounded-full shadow-2xl ring-4 ring-base-content/10 transition-all hover:ring-primary/40 sm:h-40 sm:w-40"
							aria-label="Change profile picture"
						>
							{#if profile.avatar_url}
								<img
									src={profile.avatar_url}
									alt="Profile"
									class="h-full w-full object-cover transition-transform duration-300 group-hover:scale-110"
								/>
							{:else}
								<div
									class="flex h-full w-full items-center justify-center bg-linear-to-br from-primary/30 to-accent/20"
								>
									<UserRound class="h-16 w-16 text-base-content/40 sm:h-20 sm:w-20" />
								</div>
							{/if}
							<div
								class="absolute inset-0 flex items-center justify-center bg-black/50 opacity-0 transition-opacity group-hover:opacity-100"
							>
								<Camera class="h-8 w-8 text-white" />
							</div>
						</button>

						<div class="flex w-full max-w-md flex-col items-center gap-2 pb-2">
							<div class="flex items-center gap-2">
								<span class="text-xs font-semibold uppercase tracking-widest text-base-content/40"
									>Profile</span
								>
								{#if authStore.user}
									{#if authStore.user.role === 'admin'}
										<span class="badge badge-accent badge-sm gap-1">
											<ShieldCheck class="h-3 w-3" />{roleLabel[authStore.user.role]}
										</span>
									{:else if authStore.user.role === 'trusted'}
										<span class="badge badge-info badge-sm gap-1">
											<UserCheck class="h-3 w-3" />{roleLabel[authStore.user.role]}
										</span>
									{:else}
										<span class="badge badge-ghost badge-sm">{roleLabel[authStore.user.role]}</span>
									{/if}
								{/if}
							</div>
							{#if editingName}
								<div class="flex items-center gap-2">
									<input
										type="text"
										bind:value={nameInput}
										onkeydown={handleNameKeydown}
										class="input input-sm input-soft text-2xl font-bold"
										placeholder="Your name"
									/>
									<button
										onclick={() => void saveName()}
										class="btn btn-ghost btn-sm btn-circle"
										disabled={displayNameMutation.isPending}
										aria-label="Save name"
									>
										<Check class="h-4 w-4 text-success" />
									</button>
									<button
										onclick={() => (editingName = false)}
										class="btn btn-ghost btn-sm btn-circle"
										aria-label="Cancel"
									>
										<X class="h-4 w-4 text-error" />
									</button>
								</div>
								{#if nameError}
									<p class="text-xs text-error">{nameError}</p>
								{/if}
							{:else}
								<button
									onclick={startEditName}
									class="group flex items-center gap-2"
									aria-label="Edit display name"
								>
									<h1 class="text-3xl font-bold sm:text-4xl">
										{profile.display_name || 'Set your name'}
									</h1>
									<Pencil
										class="h-4 w-4 text-base-content/30 transition-colors group-hover:text-primary"
									/>
								</button>
							{/if}
						</div>
					</div>
				{:else if profileQuery.isError}
					<div class="flex flex-col items-center gap-4 py-12 text-center">
						<CircleAlert class="h-10 w-10 text-base-content/50" />
						<p class="text-base-content/70">Failed to load profile</p>
						<button
							class="btn btn-primary btn-sm gap-2"
							onclick={() => void profileQuery.refetch()}
						>
							<RefreshCw class="h-4 w-4" />
							Try Again
						</button>
					</div>
				{/if}
			</div>
		</div>
	</div>

	{#if profile}
		<div class="px-4 pb-12 sm:px-6 lg:px-8">
			<div class="mx-auto max-w-4xl space-y-8 stagger-fade-in">
				<section>
					<h2
						class="mb-4 flex items-center gap-2 text-sm font-semibold uppercase tracking-widest text-base-content/50"
					>
						<UserCog class="h-4 w-4" />
						Account
					</h2>
					<div
						class="divide-y divide-base-300/30 overflow-hidden rounded-xl border border-base-300/40 bg-base-200/50 backdrop-blur-sm"
					>
						<div class="flex items-start gap-3 px-5 py-4">
							<div
								class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-base-300/60 text-base-content/70"
							>
								<AtSign class="h-4 w-4" />
							</div>
							<div class="min-w-0 flex-1">
								<p class="text-[10px] font-medium uppercase tracking-wider text-base-content/40">
									Username
								</p>
								{#if editingUsername}
									<div class="mt-1 flex items-center gap-2">
										<input
											type="text"
											bind:value={usernameInput}
											onkeydown={handleUsernameKeydown}
											autocomplete="username"
											class="input input-sm input-soft w-full max-w-xs"
											placeholder="username"
										/>
										<button
											onclick={() => void saveUsername()}
											class="btn btn-ghost btn-sm btn-circle"
											disabled={usernameMutation.isPending}
											aria-label="Save username"
										>
											<Check class="h-4 w-4 text-success" />
										</button>
										<button
											onclick={() => (editingUsername = false)}
											class="btn btn-ghost btn-sm btn-circle"
											aria-label="Cancel"
										>
											<X class="h-4 w-4 text-error" />
										</button>
									</div>
									{#if usernameError}
										<p class="mt-1 text-xs text-error">{usernameError}</p>
									{/if}
								{:else}
									<p class="truncate text-sm font-medium">
										@{profile.username_display ?? profile.username ?? '-'}
									</p>
								{/if}
							</div>
							{#if !editingUsername}
								<button
									onclick={startEditUsername}
									class="btn btn-ghost btn-sm btn-circle text-base-content/40 hover:text-primary"
									aria-label="Edit username"
								>
									<Pencil class="h-4 w-4" />
								</button>
							{/if}
						</div>

						<div class="flex items-start gap-3 px-5 py-4">
							<div
								class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-base-300/60 text-base-content/70"
							>
								<Mail class="h-4 w-4" />
							</div>
							<div class="min-w-0 flex-1">
								<p class="text-[10px] font-medium uppercase tracking-wider text-base-content/40">
									Email
								</p>
								{#if editingEmail}
									<div class="mt-1 flex items-center gap-2">
										<input
											type="email"
											bind:value={emailInput}
											onkeydown={handleEmailKeydown}
											autocomplete="email"
											class="input input-sm input-soft w-full max-w-xs"
											placeholder="name@example.com (optional)"
										/>
										<button
											onclick={() => void saveEmail()}
											class="btn btn-ghost btn-sm btn-circle"
											disabled={emailMutation.isPending}
											aria-label="Save email"
										>
											<Check class="h-4 w-4 text-success" />
										</button>
										<button
											onclick={() => (editingEmail = false)}
											class="btn btn-ghost btn-sm btn-circle"
											aria-label="Cancel"
										>
											<X class="h-4 w-4 text-error" />
										</button>
									</div>
									{#if emailError}
										<p class="mt-1 text-xs text-error">{emailError}</p>
									{/if}
								{:else if profile.email}
									<p class="truncate text-sm font-medium">{profile.email}</p>
								{:else}
									<p class="text-sm text-base-content/40">Not set</p>
								{/if}
							</div>
							{#if !editingEmail}
								<button
									onclick={startEditEmail}
									class="btn btn-ghost btn-sm btn-circle text-base-content/40 hover:text-primary"
									aria-label="Edit email"
								>
									<Pencil class="h-4 w-4" />
								</button>
							{/if}
						</div>

						<div class="px-5 py-4">
							<div class="flex items-center gap-3">
								<div
									class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-base-300/60 text-base-content/70"
								>
									<KeyRound class="h-4 w-4" />
								</div>
								<div class="min-w-0 flex-1">
									<p class="text-[10px] font-medium uppercase tracking-wider text-base-content/40">
										Password
									</p>
									{#if hasLocalPassword}
										<p class="text-sm font-medium tracking-widest">••••••••</p>
									{:else}
										<p class="text-sm text-base-content/40">No local password</p>
									{/if}
								</div>
								<button
									onclick={togglePasswordForm}
									class="btn btn-ghost btn-sm gap-1.5 text-base-content/60 hover:text-primary"
								>
									{#if showPasswordForm}
										Cancel
									{:else if hasLocalPassword}
										<Pencil class="h-3.5 w-3.5" /> Change
									{:else}
										<KeyRound class="h-3.5 w-3.5" /> Set a password
									{/if}
								</button>
							</div>

							{#if passwordDone && !showPasswordForm}
								<p class="mt-2 flex items-center gap-1.5 pl-12 text-xs text-success">
									<Check class="h-3.5 w-3.5" />
									{hasLocalPassword ? 'Password updated' : 'Local password set'}
								</p>
							{/if}

							{#if showPasswordForm}
								<div class="mt-3 space-y-2 pl-0 sm:pl-12">
									{#if hasLocalPassword}
										<input
											type="password"
											bind:value={currentPassword}
											autocomplete="current-password"
											class="input input-sm input-soft w-full max-w-sm"
											placeholder="Current password"
										/>
									{:else}
										<p class="text-xs text-base-content/50">
											Add a password so you can also sign in with your username.
										</p>
									{/if}
									<input
										type="password"
										bind:value={newPassword}
										autocomplete="new-password"
										class="input input-sm input-soft w-full max-w-sm"
										placeholder="New password (min 12 characters)"
									/>
									{#if passwordError}
										<p class="text-xs text-error">{passwordError}</p>
									{/if}
									<div class="flex gap-2 pt-1">
										<button
											onclick={() => void submitPassword()}
											class="btn btn-primary btn-sm glow-primary-soft gap-1.5 rounded-full"
											disabled={passwordPending ||
												!newPassword ||
												(hasLocalPassword && !currentPassword)}
										>
											{#if passwordPending}
												<span class="loading loading-spinner loading-xs"></span>
											{/if}
											{hasLocalPassword ? 'Update password' : 'Set password'}
										</button>
										<button onclick={togglePasswordForm} class="btn btn-ghost btn-sm">Cancel</button
										>
									</div>
								</div>
							{/if}
						</div>
					</div>
				</section>

				<section>
					<h2
						class="mb-4 flex items-center gap-2 text-sm font-semibold uppercase tracking-widest text-base-content/50"
					>
						<ExternalLink class="h-4 w-4" />
						Connected Services
					</h2>
					<div class="grid gap-3 sm:grid-cols-3">
						{#each visibleServices as service (service.name)}
							{@const Icon = getServiceIcon(service.name)}
							{@const profileUrl = getServiceProfileUrl(service)}
							<a
								href={profileUrl ?? undefined}
								target={profileUrl ? '_blank' : undefined}
								rel={profileUrl ? 'noopener noreferrer' : undefined}
								role={profileUrl ? undefined : 'presentation'}
								class="crate-card group rounded-xl border {getServiceBorderColor(
									service.name
								)} bg-base-200/50 p-4 backdrop-blur-sm transition-all hover:bg-base-200/80 hover:shadow-lg {profileUrl
									? 'cursor-pointer'
									: 'cursor-default'} block no-underline text-inherit"
							>
								<div class="flex items-center gap-3">
									<div
										class="flex h-10 w-10 items-center justify-center rounded-lg bg-base-300/60 {getServiceColor(
											service.name
										)}"
									>
										<Icon class="h-5 w-5" />
									</div>
									<div class="min-w-0 flex-1">
										<div class="flex items-center gap-2">
											<span class="text-sm font-semibold">{service.name}</span>
											{#if service.enabled}
												<span class="status status-success status-sm"></span>
											{:else}
												<span class="status status-error status-sm"></span>
											{/if}
											{#if profileUrl}
												<ExternalLink
													class="h-3 w-3 text-base-content/30 transition-colors group-hover:text-primary"
												/>
											{/if}
										</div>
										{#if service.enabled && service.username}
											<p class="mt-0.5 truncate text-xs text-base-content/50">
												{service.username}
											</p>
										{:else if !service.enabled}
											<p class="mt-0.5 text-xs text-base-content/30">Not connected</p>
										{/if}
									</div>
								</div>
							</a>
						{/each}
					</div>
				</section>

				<div id="scrobbling" class="scroll-mt-20">
					<ScrobblingDiscoveryCard />
				</div>

				<div id="spotify" class="scroll-mt-20">
					<SpotifyConnectionCard />
				</div>

				{#if profile.library_stats.length > 0}
					<section>
						<h2
							class="mb-4 flex items-center gap-2 text-sm font-semibold uppercase tracking-widest text-base-content/50"
						>
							<Database class="h-4 w-4" />
							Your Libraries
						</h2>
						<div class="space-y-4">
							{#each profile.library_stats as stats (stats.source)}
								{@const SourceIcon = getSourceIcon(stats.source)}
								<div
									class="crate-card overflow-hidden rounded-xl border border-base-300/40 bg-base-200/50 backdrop-blur-sm"
								>
									<div class="flex items-center gap-3 border-b border-base-300/30 px-5 py-3">
										<div
											class="flex h-8 w-8 items-center justify-center rounded-lg bg-base-300/60 {getSourceColor(
												stats.source
											)}"
										>
											<SourceIcon class="h-4 w-4" />
										</div>
										<span class="text-sm font-semibold">{stats.source}</span>
									</div>
									<div class="grid grid-cols-3 divide-x divide-base-300/30 px-1 py-4">
										<div class="flex flex-col items-center gap-1">
											<div class="flex items-center gap-1.5 text-base-content/50">
												<Disc3 class="h-3.5 w-3.5" />
												<span class="text-[10px] font-medium uppercase tracking-wider">Songs</span>
											</div>
											<span class="text-xl font-bold tabular-nums">
												{formatNumber(stats.total_tracks)}
											</span>
										</div>
										<div class="flex flex-col items-center gap-1">
											<div class="flex items-center gap-1.5 text-base-content/50">
												<Database class="h-3.5 w-3.5" />
												<span class="text-[10px] font-medium uppercase tracking-wider">Albums</span>
											</div>
											<span class="text-xl font-bold tabular-nums">
												{formatNumber(stats.total_albums)}
											</span>
										</div>
										<div class="flex flex-col items-center gap-1">
											<div class="flex items-center gap-1.5 text-base-content/50">
												<Users class="h-3.5 w-3.5" />
												<span class="text-[10px] font-medium uppercase tracking-wider">
													Artists
												</span>
											</div>
											<span class="text-xl font-bold tabular-nums">
												{formatNumber(stats.total_artists)}
											</span>
										</div>
									</div>
									{#if stats.total_size_human}
										<div
											class="flex items-center justify-center gap-2 border-t border-base-300/30 px-5 py-3"
										>
											<HardDrive class="h-3.5 w-3.5 text-base-content/40" />
											<span class="text-xs text-base-content/50">
												{stats.total_size_human} used
											</span>
										</div>
									{/if}
								</div>
							{/each}
						</div>
					</section>
				{/if}

				<section class="flex justify-center gap-3 pt-2">
					<a
						href="/settings"
						class="btn btn-outline btn-sm gap-2 rounded-full border-base-content/20 text-base-content/60 transition-all hover:border-primary hover:text-primary"
					>
						<Settings class="h-4 w-4" />
						Open Settings
					</a>
					<button
						class="btn btn-outline btn-sm gap-2 rounded-full border-base-content/20 text-base-content/60 transition-all hover:border-error hover:text-error"
						onclick={() => void logout()}
					>
						<LogOut class="h-4 w-4" />
						Sign Out
					</button>
				</section>
			</div>
		</div>
	{/if}
</div>

<dialog class="modal" class:modal-open={showAvatarModal}>
	<div class="modal-box bg-base-200 border border-base-300 max-w-sm">
		<h3 class="mb-4 text-lg font-bold">Upload Profile Picture</h3>
		<input
			type="file"
			accept="image/jpeg,image/png,image/webp,image/gif"
			class="hidden"
			bind:this={fileInput}
			onchange={handleFileSelect}
		/>
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div
			class="flex flex-col items-center justify-center gap-3 rounded-box border-2 border-dashed p-6 transition-colors cursor-pointer
				{draggingOver ? 'border-primary bg-primary/10' : 'border-base-content/20 hover:border-primary/50'}"
			ondrop={handleDrop}
			ondragover={handleDragOver}
			ondragleave={() => (draggingOver = false)}
			onclick={() => fileInput?.click()}
			onkeydown={(e) => {
				if (e.key === 'Enter' || e.key === ' ') fileInput?.click();
				if (e.key === 'Escape') closeAvatarModal();
			}}
		>
			{#if avatarPreview}
				<img
					src={avatarPreview}
					alt="Preview"
					class="h-24 w-24 rounded-full object-cover ring-2 ring-base-content/10"
				/>
				<p class="text-xs text-base-content/60">{avatarFile?.name}</p>
			{:else}
				<ImagePlus class="h-10 w-10 text-base-content/30" />
				<p class="text-sm text-base-content/60">Drag & drop an image here, or click to browse</p>
				<p class="text-xs text-base-content/40">JPEG, PNG, WebP, or GIF, max 5 MB</p>
			{/if}
		</div>
		<div class="modal-action">
			<button class="btn btn-ghost btn-sm" onclick={closeAvatarModal}>Cancel</button>
			<button
				class="btn btn-primary btn-sm"
				onclick={() => void uploadAvatar()}
				disabled={avatarMutation.isPending || !avatarFile}
			>
				{#if avatarMutation.isPending}
					<span class="loading loading-spinner loading-xs"></span>
				{/if}
				Upload</button
			>
		</div>
	</div>
	<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<form method="dialog" class="modal-backdrop" onclick={closeAvatarModal}>
		<button>close</button>
	</form>
</dialog>
