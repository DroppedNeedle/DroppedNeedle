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
	import { Eye, EyeOff } from 'lucide-svelte';
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
	// auto-select a tab once providers load, but never override a user's choice
	let tabInitialised = false;
	$effect(() => {
		if (tabInitialised || !providersQuery.isSuccess) return;
		tabInitialised = true;
		if (!providers.local && providers.plex) activeTab = 'plex';
		else if (!providers.local && providers.jellyfin) activeTab = 'jellyfin';
		else if (!providers.local && providers.oidc) activeTab = 'oidc';
	});

	let username = $state('');
	let password = $state('');
	let showPassword = $state(false);
	let localError = $state<string | null>(null);
	const localLogin = createLocalLoginMutation();

	let jfUsername = $state('');
	let jfPassword = $state('');
	let jfShowPassword = $state(false);
	let jfError = $state<string | null>(null);
	const jellyfinLogin = createJellyfinLoginMutation();

	// plex PIN request is a mutation; authorisation is polled imperatively
	let plexLoading = $state(false);
	let plexError = $state<string | null>(null);
	let plexPollInterval: ReturnType<typeof setInterval> | null = null;
	const plexPin = createPlexPinMutation();

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
			storeSession(await localLogin.mutateAsync({ username, password }));
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
	<title>Sign in - DroppedNeedle</title>
</svelte:head>

<div class="login-wrap grain flex min-h-screen items-center justify-center p-4">
	<div class="w-full max-w-md">
		<div class="login-brand">
			<img src="/logo_icon.png" alt="" aria-hidden="true" class="login-mark" />
			<p class="login-eyebrow">Audiophile grade</p>
			<h1 class="login-wordmark">DroppedNeedle</h1>
			<div class="login-rule" aria-hidden="true"></div>
			<p class="login-sub">Sign in to continue</p>
		</div>

		<div
			class="rounded-2xl border border-base-content/8 bg-base-200/50 p-6 backdrop-blur-sm sm:p-7"
		>
			{#if availableTabs.length > 1}
				<div
					class="mb-6 flex flex-wrap items-center justify-center gap-1 rounded-full border border-base-content/8 bg-base-100/50 p-1"
					role="tablist"
					aria-label="Sign-in providers"
				>
					{#each availableTabs as tab (tab)}
						<button
							class="pill-tab"
							class:pill-tab-active={activeTab === tab}
							role="tab"
							aria-selected={activeTab === tab}
							onclick={() => (activeTab = tab)}
						>
							{#if tab === 'plex'}<PlexIcon class="h-4 w-4" style="color: rgb(var(--brand-plex))" />
							{:else if tab === 'jellyfin'}<JellyfinIcon class="h-4 w-4 text-info" />
							{/if}
							{tab === 'local'
								? 'Username'
								: tab === 'oidc'
									? 'SSO'
									: tab.charAt(0).toUpperCase() + tab.slice(1)}
						</button>
					{/each}
				</div>
			{/if}

			{#if activeTab === 'local'}
				<form
					onsubmit={(e) => {
						e.preventDefault();
						void handleLocalLogin();
					}}
					class="flex flex-col gap-4"
				>
					<div class="flex flex-col gap-1.5">
						<label class="field-label" for="login-username">Username</label>
						<input
							id="login-username"
							type="text"
							class="input w-full rounded-full border-base-content/12 bg-base-100/60"
							placeholder="Username"
							bind:value={username}
							required
							autocomplete="username"
						/>
					</div>
					<div class="flex flex-col gap-1.5">
						<span class="field-label">Password</span>
						<label
							class="input flex w-full items-center gap-2 rounded-full border-base-content/12 bg-base-100/60"
						>
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
								class="opacity-50 transition-opacity hover:opacity-100"
								aria-label="Toggle password visibility"
							>
								{#if showPassword}<EyeOff class="h-4 w-4" />{:else}<Eye class="h-4 w-4" />{/if}
							</button>
						</label>
					</div>
					{#if localError}
						<div class="alert alert-error rounded-2xl py-2 text-sm">{localError}</div>
					{/if}
					<button
						type="submit"
						class="btn btn-primary w-full rounded-full"
						disabled={localLogin.isPending}
					>
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
					<div class="mb-1 flex items-center gap-2">
						<JellyfinIcon class="h-5 w-5 text-info" />
						<span class="text-sm font-medium">Sign in with your Jellyfin account</span>
					</div>
					<div class="flex flex-col gap-1.5">
						<label class="field-label" for="login-jf-username">Username</label>
						<input
							id="login-jf-username"
							type="text"
							class="input w-full rounded-full border-base-content/12 bg-base-100/60"
							placeholder="Jellyfin username"
							bind:value={jfUsername}
							required
							autocomplete="username"
						/>
					</div>
					<div class="flex flex-col gap-1.5">
						<span class="field-label">Password</span>
						<label
							class="input flex w-full items-center gap-2 rounded-full border-base-content/12 bg-base-100/60"
						>
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
								class="opacity-50 transition-opacity hover:opacity-100"
								aria-label="Toggle password visibility"
							>
								{#if jfShowPassword}<EyeOff class="h-4 w-4" />{:else}<Eye class="h-4 w-4" />{/if}
							</button>
						</label>
					</div>
					{#if jfError}
						<div class="alert alert-error rounded-2xl py-2 text-sm">{jfError}</div>
					{/if}
					<button
						type="submit"
						class="btn btn-primary w-full rounded-full"
						disabled={jellyfinLogin.isPending}
					>
						{#if jellyfinLogin.isPending}<span class="loading loading-spinner loading-sm"
							></span>{/if}
						Sign in with Jellyfin
					</button>
				</form>
			{:else if activeTab === 'plex'}
				<div class="flex flex-col gap-4">
					<div class="mb-1 flex items-center gap-2">
						<PlexIcon class="h-5 w-5" style="color: rgb(var(--brand-plex))" />
						<span class="text-sm font-medium">Sign in with your Plex account</span>
					</div>
					<p class="text-sm text-base-content/60">
						A Plex login window will open. Sign in there and return to this page.
					</p>
					{#if plexError}
						<div class="alert alert-error rounded-2xl py-2 text-sm">{plexError}</div>
					{/if}
					<button
						class="btn btn-primary w-full gap-2 rounded-full"
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
						<div class="alert alert-error rounded-2xl py-2 text-sm">{oidcError}</div>
					{/if}
					<button
						class="btn btn-primary w-full rounded-full"
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

<style>
	.login-wrap {
		--grain-opacity: 0.1;
		background:
			radial-gradient(
				circle at 50% -8rem,
				oklch(from var(--color-primary) l c h / 0.08),
				transparent 24rem
			),
			radial-gradient(
				circle at 85% 110%,
				oklch(from var(--color-accent) l c h / 0.05),
				transparent 26rem
			),
			var(--color-base-100);
	}

	.login-brand {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.5rem;
		margin-bottom: 2rem;
	}

	.login-mark {
		height: 3rem;
		width: auto;
		margin-bottom: 0.25rem;
		opacity: 0.9;
	}

	.login-eyebrow {
		font-family: var(--font-mono);
		font-size: 0.62rem;
		font-weight: 700;
		letter-spacing: 0.28em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.45);
	}

	.login-wordmark {
		font-family: var(--font-display);
		font-weight: 800;
		font-size: clamp(2.75rem, 14vw, 4rem);
		line-height: 0.85;
		letter-spacing: 0.01em;
		color: oklch(from var(--color-base-content) l c h / 0.95);
		text-shadow: 0 2px 1px rgb(0 0 0 / 0.4);
	}

	.login-rule {
		height: 2px;
		width: 7rem;
		border-radius: 999px;
		background: linear-gradient(
			to right,
			transparent,
			oklch(from var(--color-primary) l c h / 0.6),
			oklch(from var(--color-accent) l c h / 0.6),
			transparent
		);
	}

	.login-sub {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.5);
	}

	.pill-tab {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		padding: 0.42rem 0.95rem;
		border-radius: 999px;
		font-family: var(--font-mono);
		font-size: 0.68rem;
		font-weight: 700;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.5);
		background: transparent;
		border: none;
		cursor: pointer;
		transition:
			background var(--dn-dur-fast, 0.15s) ease,
			color var(--dn-dur-fast, 0.15s) ease;
	}
	.pill-tab:hover {
		color: oklch(from var(--color-base-content) l c h / 0.8);
	}
	.pill-tab-active {
		background: var(--color-primary);
		color: var(--color-primary-content);
	}
	.pill-tab-active:hover {
		color: var(--color-primary-content);
	}

	.field-label {
		margin-left: 1rem;
		font-family: var(--font-mono);
		font-size: 0.62rem;
		font-weight: 700;
		letter-spacing: 0.18em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.5);
	}
</style>
