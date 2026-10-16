<script lang="ts">
	import '../../auth.css';
	import { goto } from '$app/navigation';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { ApiError } from '$lib/api/client';
	import { getAuthProvidersQuery } from '$lib/queries/auth/AuthProvidersQuery.svelte';
	import {
		createJellyfinLoginMutation,
		createLocalLoginMutation,
		createOidcAuthorizeMutation
	} from '$lib/queries/auth/AuthMutations.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { outsideLink } from '$lib/components/outsideLink';
	import { createPlexStartMutation } from '$lib/queries/plex/PlexAuthMutations.svelte';
	import { pollPlexFlow } from '$lib/queries/plex/PlexFlowApi';
	import { toAuthUser, type AuthProviders, type SessionUserLike } from '$lib/queries/auth/types';
	import { onDestroy } from 'svelte';
	import { Eye, EyeOff } from 'lucide-svelte';
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Field from '$lib/ui/Field.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import TextField from '$lib/ui/TextField.svelte';

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
	const plexPin = createPlexStartMutation();

	let oidcLoading = $state(false);
	let oidcError = $state<string | null>(null);
	const oidcAuthorize = createOidcAuthorizeMutation();

	onDestroy(() => {
		if (plexPollInterval) clearInterval(plexPollInterval);
	});

	function storeSession(data: { user: SessionUserLike }) {
		authStore.setUser(toAuthUser(data.user));
		goto(withBasePath('/'));
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
			const { pin_id, authorize_url } = await plexPin.mutateAsync({ purpose: 'login' });
			window.open(authorize_url, '_blank', 'width=800,height=600');

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
			const { authorize_url } = await oidcAuthorize.mutateAsync();
			// The address starts with whatever the identity provider's discovery
			// document names, and assigning a script address here would run it.
			const authorizeLink = outsideLink(authorize_url);
			if (!authorizeLink) {
				oidcError = 'SSO is not configured';
				oidcLoading = false;
				return;
			}
			window.location.href = authorizeLink;
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

<div class="login-wrap grain min-h-screen flex items-center justify-center p-4">
	<div class="w-full max-w-md">
		<div class="login-brand">
			<img
				src={withBasePath('/logo_icon.png')}
				alt=""
				aria-hidden="true"
				class="dn-logo login-mark"
			/>
			<h1 class="login-wordmark">DroppedNeedle</h1>
			<div class="login-rule" aria-hidden="true"></div>
			<p class="login-sub">Sign in to continue</p>
		</div>

		<Card variant="bordered" padding="none">
			<div>
				{#if availableTabs.length > 1}
					<div class="flex border-b border-base-300 px-2 pt-2">
						{#each availableTabs as tab (tab)}
							<button
								type="button"
								class="tab-btn"
								class:tab-btn-active={activeTab === tab}
								onclick={() => (activeTab = tab)}
							>
								{#if tab === 'plex'}<PlexIcon class="h-4 w-4 text-[rgb(var(--brand-plex))]" />
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

				<div class="p-6">
					{#if activeTab === 'local'}
						<form
							onsubmit={(e) => {
								e.preventDefault();
								void handleLocalLogin();
							}}
							class="flex flex-col gap-4"
						>
							<TextField
								label="Username"
								placeholder="Username"
								bind:value={username}
								required
								autocomplete="username"
							/>
							<div>
								{@render passwordField(
									'Password',
									() => password,
									(value) => (password = value),
									showPassword,
									() => (showPassword = !showPassword)
								)}
								<div class="mt-1 flex justify-end">
									<a
										href={withBasePath('/recover-password')}
										class="link link-primary text-xs font-medium"
									>
										Forgot password?
									</a>
								</div>
							</div>
							{#if localError}
								<div class="alert alert-error py-2 text-sm">{localError}</div>
							{/if}
							<Button type="submit" variant="primary" class="w-full" loading={localLogin.isPending}>
								Sign in
							</Button>
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
							<TextField
								label="Username"
								placeholder="Jellyfin username"
								bind:value={jfUsername}
								required
								autocomplete="username"
							/>
							{@render passwordField(
								'Password',
								() => jfPassword,
								(value) => (jfPassword = value),
								jfShowPassword,
								() => (jfShowPassword = !jfShowPassword)
							)}
							{#if jfError}
								<div class="alert alert-error py-2 text-sm">{jfError}</div>
							{/if}
							<Button
								type="submit"
								variant="primary"
								class="w-full"
								loading={jellyfinLogin.isPending}
							>
								Sign in with Jellyfin
							</Button>
						</form>
					{:else if activeTab === 'plex'}
						<div class="flex flex-col gap-4">
							<div class="flex items-center gap-2 mb-1">
								<PlexIcon class="h-5 w-5 text-[rgb(var(--brand-plex))]" />
								<span class="text-sm font-medium">Sign in with your Plex account</span>
							</div>
							<p class="text-sm text-base-content/60">
								A Plex login window will open. Sign in there and return to this page.
							</p>
							{#if plexError}
								<div class="alert alert-error py-2 text-sm">{plexError}</div>
							{/if}
							<Button
								variant="primary"
								class="w-full"
								loading={plexLoading}
								onclick={() => void handlePlexLogin()}
							>
								{#if plexLoading}
									Waiting for Plex…
								{:else}
									<PlexIcon class="h-4 w-4 text-current" />
									Continue with Plex
								{/if}
							</Button>
						</div>
					{:else if activeTab === 'oidc'}
						<div class="flex flex-col gap-4">
							<p class="text-sm text-base-content/60">
								Sign in using your organisation's single sign-on provider.
							</p>
							{#if oidcError}
								<div class="alert alert-error py-2 text-sm">{oidcError}</div>
							{/if}
							<Button
								variant="primary"
								class="w-full"
								loading={oidcLoading}
								onclick={() => void handleOidcLogin()}
							>
								Continue with SSO
							</Button>
						</div>
					{/if}
				</div>
			</div>
		</Card>
	</div>
</div>

{#snippet passwordField(
	label: string,
	get: () => string,
	set: (value: string) => void,
	shown: boolean,
	toggle: () => void
)}
	<Field {label}>
		{#snippet children(control)}
			<div class="join w-full">
				<Input
					{...control}
					{label}
					type={shown ? 'text' : 'password'}
					class="join-item w-full"
					placeholder="Password"
					required
					autocomplete="current-password"
					bind:value={get, set}
				/>
				<IconButton
					label="Show password"
					icon={shown ? EyeOff : Eye}
					variant="neutral"
					shape="square"
					class="join-item"
					aria-pressed={shown}
					onclick={toggle}
				/>
			</div>
		{/snippet}
	</Field>
{/snippet}

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

	.login-wrap {
		--grain-opacity: 0.1;
		position: relative;
		isolation: isolate;
		background:
			radial-gradient(
				circle at 50% -8rem,
				oklch(from var(--color-primary) l c h / 0.08),
				transparent 22rem
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
</style>
