<script lang="ts">
	import '../../auth.css';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { ApiError } from '$lib/api/errors';
	import { getAuthProvidersQuery } from '$lib/features/auth/queries/AuthProvidersQuery.svelte';
	import {
		createJellyfinLoginMutation,
		createLocalLoginMutation,
		createOidcAuthorizeMutation
	} from '$lib/features/auth/queries/AuthMutations.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { providerUnavailableMessage } from '$lib/utils/serviceError';
	import { takeSignInReturn } from '$lib/utils/signInReturn';
	import { outsideLink } from '$lib/ui/outsideLink';
	import { createPlexStartMutation } from '$lib/features/settings/queries/plex.svelte';
	import { pollPlexFlow } from '$lib/features/settings/queries/plex.svelte';
	import {
		toAuthUser,
		type AuthProviders,
		type SessionUserLike
	} from '$lib/features/auth/queries/types';
	import { onDestroy } from 'svelte';
	import JellyfinIcon from '$lib/media/icons/JellyfinIcon.svelte';
	import PlexIcon from '$lib/media/icons/PlexIcon.svelte';
	import { Link2, UserRound } from 'lucide-svelte';
	import AuthCard from '$lib/ui/AuthCard.svelte';
	import AuthFrame from '$lib/ui/AuthFrame.svelte';
	import Button from '$lib/ui/Button.svelte';
	import PasswordField from '$lib/ui/PasswordField.svelte';
	import TextField from '$lib/ui/TextField.svelte';

	type Tab = 'local' | 'plex' | 'jellyfin' | 'oidc';

	const DEFAULT_PROVIDERS: AuthProviders = {
		local: true,
		plex: false,
		jellyfin: false,
		oidc: false,
		oidc_auto_redirect: false
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
	const plexPin = createPlexStartMutation();

	let oidcLoading = $state(false);
	let oidcError = $state<string | null>(null);
	const oidcAuthorize = createOidcAuthorizeMutation();

	onDestroy(() => {
		if (plexPollInterval) clearInterval(plexPollInterval);
	});

	function storeSession(data: { user: SessionUserLike }) {
		authStore.setUser(toAuthUser(data.user));
		goto(withBasePath(takeSignInReturn()));
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

	/** What to say when the server or a provider asked this sign-in to wait. */
	function waitMessage(error: unknown): string | null {
		if (error instanceof ApiError && error.status === 429) {
			return 'Too many attempts. Wait a moment and try again.';
		}
		return providerUnavailableMessage(error);
	}

	async function handlePlexLogin() {
		plexError = null;
		plexLoading = true;
		if (plexPollInterval) clearInterval(plexPollInterval);
		try {
			const { pin_id, authorize_url } = await plexPin.mutateAsync({ purpose: 'login' });
			const authorizeLink = outsideLink(authorize_url);
			if (!authorizeLink) {
				plexError = 'Sign-in provider address refused';
				plexLoading = false;
				return;
			}
			window.open(authorizeLink, '_blank', 'width=800,height=600');

			plexPollInterval = setInterval(async () => {
				try {
					const data = await pollPlexFlow('login', pin_id);
					if (data.completed === false) return;
					clearInterval(plexPollInterval!);
					plexLoading = false;
					if (data.user) {
						storeSession({ user: data.user });
					}
				} catch (e) {
					clearInterval(plexPollInterval!);
					plexLoading = false;
					plexError =
						providerUnavailableMessage(e) ??
						(e instanceof ApiError ? 'Plex access denied' : 'Plex login failed');
				}
			}, 2000);
		} catch (e) {
			plexError = waitMessage(e) ?? 'Plex login unavailable';
			plexLoading = false;
		}
	}

	async function handleOidcLogin() {
		oidcError = null;
		oidcLoading = true;
		try {
			const { authorize_url } = await oidcAuthorize.mutateAsync();
			// The address starts with whatever the identity provider's discovery
			// document names, and assigning a script address here would run it.
			const authorizeLink = outsideLink(authorize_url);
			if (!authorizeLink) {
				oidcError = 'Sign-in provider address refused';
				oidcLoading = false;
				return;
			}
			window.location.href = authorizeLink;
		} catch (e) {
			oidcError = waitMessage(e) ?? 'SSO is not configured';
			oidcLoading = false;
		}
	}

	// With "go straight to single sign-on" on, the page starts it once on
	// load; `?local` shows the sign-in methods instead, for an admin who
	// can't use single sign-on. Once per load, so a refused start shows
	// its error rather than trying again.
	let autoRedirected = false;
	$effect(() => {
		if (autoRedirected || !providersQuery.isSuccess) return;
		if (!providers.oidc_auto_redirect || page.url.searchParams.has('local')) return;
		autoRedirected = true;
		activeTab = 'oidc';
		void handleOidcLogin();
	});

	const availableTabs = $derived(
		(['local', 'plex', 'jellyfin', 'oidc'] as Tab[]).filter((t) => providers[t])
	);

	const TAB_LABELS: Record<Tab, string> = {
		local: 'Username',
		plex: 'Plex',
		jellyfin: 'Jellyfin',
		oidc: 'SSO'
	};

	const activeError = $derived(
		{ local: localError, plex: plexError, jellyfin: jfError, oidc: oidcError }[activeTab]
	);
</script>

<svelte:head>
	<title>Sign in - DroppedNeedle</title>
</svelte:head>

<AuthFrame kicker="Sign in to continue">
	<AuthCard error={activeError} tabs={availableTabs.length > 1 ? methodTabs : undefined}>
		{#if activeTab === 'local'}
			<form
				onsubmit={(e) => {
					e.preventDefault();
					void handleLocalLogin();
				}}
				class="flex flex-col gap-5"
			>
				<TextField
					label="Username"
					placeholder="Username"
					bind:value={username}
					required
					autocomplete="username"
				/>
				<div>
					<PasswordField
						label="Password"
						placeholder="Password"
						required
						bind:value={password}
						bind:shown={showPassword}
					/>
					<div class="mt-2 flex justify-end">
						<a href={withBasePath('/recover-password')} class="text-sm">Forgot password?</a>
					</div>
				</div>
				<Button
					type="submit"
					variant="primary"
					size="display"
					class="w-full"
					loading={localLogin.isPending}
				>
					Sign in
				</Button>
			</form>
		{:else if activeTab === 'jellyfin'}
			<form
				onsubmit={(e) => {
					e.preventDefault();
					void handleJellyfinLogin();
				}}
				class="flex flex-col gap-5"
			>
				<h2 class="login-method">Sign in with your Jellyfin account</h2>
				<TextField
					label="Username"
					placeholder="Jellyfin username"
					bind:value={jfUsername}
					required
					autocomplete="username"
				/>
				<PasswordField
					label="Password"
					placeholder="Password"
					required
					bind:value={jfPassword}
					bind:shown={jfShowPassword}
				/>
				<Button
					type="submit"
					variant="primary"
					size="display"
					class="w-full"
					loading={jellyfinLogin.isPending}
				>
					Sign in with Jellyfin
				</Button>
			</form>
		{:else if activeTab === 'plex'}
			<div class="flex flex-col gap-5">
				<div>
					<h2 class="login-method">Sign in with your Plex account</h2>
					<p class="login-line">
						A Plex login window will open. Sign in there and return to this page.
					</p>
				</div>
				<Button
					variant="primary"
					size="display"
					class="w-full"
					loading={plexLoading}
					loadingLabel="Waiting for Plex…"
					onclick={() => void handlePlexLogin()}
				>
					<PlexIcon class="size-5 text-current" />
					Continue with Plex
				</Button>
			</div>
		{:else if activeTab === 'oidc'}
			<div class="flex flex-col gap-5">
				<p class="login-line">Sign in using your organisation's single sign-on provider.</p>
				<Button
					variant="primary"
					size="display"
					class="w-full"
					loading={oidcLoading}
					onclick={() => void handleOidcLogin()}
				>
					Continue with SSO
				</Button>
			</div>
		{/if}
	</AuthCard>
</AuthFrame>

{#snippet methodTabs()}
	{#each availableTabs as tab (tab)}
		<button
			type="button"
			class="dn-auth-tab"
			aria-pressed={activeTab === tab}
			onclick={() => (activeTab = tab)}
		>
			{#if tab === 'plex'}<PlexIcon class="text-current" />
			{:else if tab === 'jellyfin'}<JellyfinIcon class="text-current" />
			{:else if tab === 'oidc'}<Link2 aria-hidden="true" />
			{:else}<UserRound aria-hidden="true" />
			{/if}
			<span class="dn-auth-tab__label">{TAB_LABELS[tab]}</span>
		</button>
	{/each}
{/snippet}

<style>
	.login-method {
		font-family: var(--font-display);
		font-size: 1.625rem;
		font-weight: 400;
		line-height: 1;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		color: var(--dn-paper-content);
	}
	.login-line {
		margin-top: 0.625rem;
		font-size: 0.9375rem;
		line-height: 1.45;
		color: var(--dn-muted);
	}
	.login-line:first-child {
		margin-top: 0;
	}
</style>
