<script lang="ts">
	import { goto } from '$app/navigation';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { api, ApiError } from '$lib/api/client';
	import { getAuthProvidersQuery } from '$lib/queries/auth/AuthProvidersQuery.svelte';
	import {
		createJellyfinLoginMutation,
		createLocalLoginMutation,
		createOidcAuthorizeMutation,
		createPlexPinMutation
	} from '$lib/queries/auth/AuthMutations.svelte';
	import { AUTH_ENDPOINTS } from '$lib/queries/auth/endpoints';
	import {
		toAuthUser,
		type AuthProviders,
		type AuthSessionResponse,
		type PlexPollResponse
	} from '$lib/queries/auth/types';
	import { onDestroy } from 'svelte';
	import { Music, Eye, EyeOff } from 'lucide-svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';

	type Tab = 'local' | 'plex' | 'jellyfin' | 'oidc';

	const DEFAULT_PROVIDERS: AuthProviders = {
		local: true,
		plex: false,
		jellyfin: false,
		oidc: false
	};

	const providersQuery = getAuthProvidersQuery();
	const providers = $derived(providersQuery.data ?? DEFAULT_PROVIDERS);

	let activeTab = $state<Tab>('local');
	// Auto-select a tab once providers load, but never override a user's choice.
	let tabInitialised = false;
	$effect(() => {
		if (tabInitialised || !providersQuery.isSuccess) return;
		tabInitialised = true;
		if (!providers.local && providers.plex) activeTab = 'plex';
		else if (!providers.local && providers.jellyfin) activeTab = 'jellyfin';
		else if (!providers.local && providers.oidc) activeTab = 'oidc';
	});

	// Local login
	let email = $state('');
	let password = $state('');
	let showPassword = $state(false);
	let localError = $state<string | null>(null);
	const localLogin = createLocalLoginMutation();

	// Jellyfin login
	let jfUsername = $state('');
	let jfPassword = $state('');
	let jfShowPassword = $state(false);
	let jfError = $state<string | null>(null);
	const jellyfinLogin = createJellyfinLoginMutation();

	// Plex login: the PIN request is a mutation; authorisation is polled imperatively.
	let plexLoading = $state(false);
	let plexError = $state<string | null>(null);
	let plexPollInterval: ReturnType<typeof setInterval> | null = null;
	const plexPin = createPlexPinMutation();

	// OIDC
	let oidcLoading = $state(false);
	let oidcError = $state<string | null>(null);
	const oidcAuthorize = createOidcAuthorizeMutation();

	onDestroy(() => {
		if (plexPollInterval) clearInterval(plexPollInterval);
	});

	function storeSession(data: AuthSessionResponse) {
		authStore.setUser(toAuthUser(data.user));
		goto('/');
	}

	async function handleLocalLogin() {
		localError = null;
		try {
			storeSession(await localLogin.mutateAsync({ email, password }));
		} catch (e) {
			localError = e instanceof ApiError ? e.message : 'Could not reach the server';
		}
	}

	async function handleJellyfinLogin() {
		jfError = null;
		try {
			storeSession(await jellyfinLogin.mutateAsync({ username: jfUsername, password: jfPassword }));
		} catch (e) {
			jfError = e instanceof ApiError ? e.message : 'Could not reach the server';
		}
	}

	async function handlePlexLogin() {
		plexError = null;
		plexLoading = true;
		if (plexPollInterval) clearInterval(plexPollInterval);
		try {
			const { pin_id, auth_url } = await plexPin.mutateAsync();
			window.open(auth_url, '_blank', 'width=800,height=600');

			plexPollInterval = setInterval(async () => {
				try {
					const data = await api.global.get<PlexPollResponse>(AUTH_ENDPOINTS.plexPoll(pin_id));
					if (data.completed === false) return;
					clearInterval(plexPollInterval!);
					plexLoading = false;
					if (data.user) storeSession({ user: data.user });
				} catch (e) {
					clearInterval(plexPollInterval!);
					plexLoading = false;
					plexError = e instanceof ApiError ? 'Plex access denied' : 'Plex login failed';
				}
			}, 2000);
		} catch {
			plexError = 'Plex login unavailable';
			plexLoading = false;
		}
	}

	async function handleOidcLogin() {
		oidcError = null;
		oidcLoading = true;
		try {
			const { redirect_url } = await oidcAuthorize.mutateAsync();
			window.location.href = redirect_url;
		} catch {
			oidcError = 'SSO is not configured';
			oidcLoading = false;
		}
	}

	const availableTabs = $derived(
		(['local', 'plex', 'jellyfin', 'oidc'] as Tab[]).filter((t) => providers[t])
	);
</script>

<svelte:head>
	<title>Sign in - Musicseerr</title>
</svelte:head>

<div class="min-h-screen bg-base-100 flex items-center justify-center p-4">
	<div class="w-full max-w-md">
		<div class="flex flex-col items-center mb-8 gap-3">
			<div class="bg-primary/10 rounded-full p-4">
				<Music class="h-10 w-10 text-primary" />
			</div>
			<h1 class="text-3xl font-bold">Musicseerr</h1>
			<p class="text-base-content/60 text-sm">Sign in to continue</p>
		</div>

		<div class="bg-base-200 rounded-box shadow-lg border border-base-300">
			{#if availableTabs.length > 1}
				<div class="flex border-b border-base-300 px-2 pt-2">
					{#each availableTabs as tab (tab)}
						<button
							class="tab-btn"
							class:tab-btn-active={activeTab === tab}
							onclick={() => (activeTab = tab)}
						>
							{#if tab === 'plex'}<PlexIcon class="h-4 w-4" style="color: rgb(var(--brand-plex))" />
							{:else if tab === 'jellyfin'}<JellyfinIcon class="h-4 w-4 text-info" />
							{/if}
							{tab === 'local'
								? 'Email'
								: tab === 'oidc'
									? 'SSO'
									: tab.charAt(0).toUpperCase() + tab.slice(1)}
						</button>
					{/each}
				</div>
			{/if}

			<div class="p-6">
				{#if activeTab === 'local'}
					<form
						onsubmit={(e) => {
							e.preventDefault();
							void handleLocalLogin();
						}}
						class="flex flex-col gap-4"
					>
						<fieldset class="fieldset">
							<legend class="fieldset-legend">Email</legend>
							<input
								type="email"
								class="input input-bordered w-full"
								placeholder="you@example.com"
								bind:value={email}
								required
								autocomplete="email"
							/>
						</fieldset>
						<fieldset class="fieldset">
							<legend class="fieldset-legend">Password</legend>
							<label class="input input-bordered flex items-center gap-2 w-full">
								{#if showPassword}
									<input
										type="text"
										class="grow"
										placeholder="Password"
										bind:value={password}
										required
										autocomplete="current-password"
									/>
								{:else}
									<input
										type="password"
										class="grow"
										placeholder="Password"
										bind:value={password}
										required
										autocomplete="current-password"
									/>
								{/if}
								<button
									type="button"
									onclick={() => (showPassword = !showPassword)}
									class="opacity-50 hover:opacity-100 transition-opacity"
									aria-label="Toggle password visibility"
								>
									{#if showPassword}<EyeOff class="h-4 w-4" />{:else}<Eye class="h-4 w-4" />{/if}
								</button>
							</label>
						</fieldset>
						{#if localError}
							<div class="alert alert-error py-2 text-sm">{localError}</div>
						{/if}
						<button type="submit" class="btn btn-primary w-full" disabled={localLogin.isPending}>
							{#if localLogin.isPending}<span class="loading loading-spinner loading-sm"></span>{/if}
							Sign in
						</button>
					</form>
				{:else if activeTab === 'jellyfin'}
					<form
						onsubmit={(e) => {
							e.preventDefault();
							void handleJellyfinLogin();
						}}
						class="flex flex-col gap-4"
					>
						<div class="flex items-center gap-2 mb-1">
							<JellyfinIcon class="h-5 w-5 text-info" />
							<span class="text-sm font-medium">Sign in with your Jellyfin account</span>
						</div>
						<fieldset class="fieldset">
							<legend class="fieldset-legend">Username</legend>
							<input
								type="text"
								class="input input-bordered w-full"
								placeholder="Jellyfin username"
								bind:value={jfUsername}
								required
								autocomplete="username"
							/>
						</fieldset>
						<fieldset class="fieldset">
							<legend class="fieldset-legend">Password</legend>
							<label class="input input-bordered flex items-center gap-2 w-full">
								{#if jfShowPassword}
									<input
										type="text"
										class="grow"
										placeholder="Password"
										bind:value={jfPassword}
										required
										autocomplete="current-password"
									/>
								{:else}
									<input
										type="password"
										class="grow"
										placeholder="Password"
										bind:value={jfPassword}
										required
										autocomplete="current-password"
									/>
								{/if}
								<button
									type="button"
									onclick={() => (jfShowPassword = !jfShowPassword)}
									class="opacity-50 hover:opacity-100 transition-opacity"
									aria-label="Toggle password visibility"
								>
									{#if jfShowPassword}<EyeOff class="h-4 w-4" />{:else}<Eye class="h-4 w-4" />{/if}
								</button>
							</label>
						</fieldset>
						{#if jfError}
							<div class="alert alert-error py-2 text-sm">{jfError}</div>
						{/if}
						<button type="submit" class="btn btn-primary w-full" disabled={jellyfinLogin.isPending}>
							{#if jellyfinLogin.isPending}<span class="loading loading-spinner loading-sm"></span>{/if}
							Sign in with Jellyfin
						</button>
					</form>
				{:else if activeTab === 'plex'}
					<div class="flex flex-col gap-4">
						<div class="flex items-center gap-2 mb-1">
							<PlexIcon class="h-5 w-5" style="color: rgb(var(--brand-plex))" />
							<span class="text-sm font-medium">Sign in with your Plex account</span>
						</div>
						<p class="text-sm text-base-content/60">
							A Plex login window will open. Sign in there and return to this page.
						</p>
						{#if plexError}
							<div class="alert alert-error py-2 text-sm">{plexError}</div>
						{/if}
						<button
							class="btn btn-primary w-full gap-2"
							onclick={() => void handlePlexLogin()}
							disabled={plexLoading}
						>
							{#if plexLoading}
								<span class="loading loading-spinner loading-sm"></span>
								Waiting for Plex…
							{:else}
								<PlexIcon class="h-4 w-4" style="color: currentColor" />
								Continue with Plex
							{/if}
						</button>
					</div>
				{:else if activeTab === 'oidc'}
					<div class="flex flex-col gap-4">
						<p class="text-sm text-base-content/60">
							Sign in using your organisation's single sign-on provider.
						</p>
						{#if oidcError}
							<div class="alert alert-error py-2 text-sm">{oidcError}</div>
						{/if}
						<button
							class="btn btn-primary w-full"
							onclick={() => void handleOidcLogin()}
							disabled={oidcLoading}
						>
							{#if oidcLoading}<span class="loading loading-spinner loading-sm"></span>{/if}
							Continue with SSO
						</button>
					</div>
				{/if}
			</div>
		</div>
	</div>
</div>

<style>
	.tab-btn {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		padding: 0.5rem 0.85rem;
		font-size: 0.875rem;
		font-weight: 500;
		color: oklch(from var(--color-base-content) l c h / 0.45);
		border-bottom: 2px solid transparent;
		transition: all 0.15s ease;
		cursor: pointer;
		background: none;
		border-top: none;
		border-left: none;
		border-right: none;
		margin-bottom: -1px;
	}
	.tab-btn:hover {
		color: oklch(from var(--color-base-content) l c h / 0.7);
	}
	.tab-btn-active {
		color: oklch(from var(--color-primary) l c h / 1);
		border-bottom-color: oklch(from var(--color-primary) l c h / 1);
	}
</style>
