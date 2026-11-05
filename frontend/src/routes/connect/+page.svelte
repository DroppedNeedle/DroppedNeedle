<script lang="ts">
	// App connect: a native app opened this tab to get an app password for
	// the signed-in user. The page shows what is asked (the app's name comes
	// from the server, never from the address) and hands the result back to
	// the app only after the user allows it here.
	import '../../auth.css';
	import { page } from '$app/state';
	import { ApiError } from '$lib/api/errors';
	import {
		createAuthorizeAppConnectMutation,
		getAppConnectRequestQuery,
		type AppConnectQuery
	} from '$lib/features/profile/queries/connect-apps.svelte';
	import type { ConnectRequest } from '$lib/features/profile/queries/connect-apps.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { appAddress, framed, handBack } from '$lib/features/auth/state/appHandBack';
	import { KeyRound, TriangleAlert } from 'lucide-svelte';
	import AuthCard from '$lib/ui/AuthCard.svelte';
	import AuthFrame from '$lib/ui/AuthFrame.svelte';
	import Button from '$lib/ui/Button.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';

	// What the app asked for, as it put it in the address; the server checks
	// every field.
	function readRequest(): ConnectRequest {
		const params = page.url.searchParams;
		const field = (key: string) => params.get(key) ?? '';
		return {
			response_type: field('response_type'),
			client_id: field('client_id'),
			redirect_uri: field('redirect_uri'),
			code_challenge: field('code_challenge'),
			code_challenge_method: field('code_challenge_method'),
			state: field('state'),
			name: params.get('name')
		};
	}

	function asQuery({ name, ...rest }: ConnectRequest): AppConnectQuery {
		return name === null || name === undefined ? rest : { ...rest, name };
	}

	const request = readRequest();
	const signedInAs = $derived(
		authStore.user?.username_display ?? authStore.user?.username ?? authStore.user?.display_name
	);

	// Inside another page nothing is asked: a frame could click Allow.
	const inFrame = framed();
	const requestQuery = getAppConnectRequestQuery(() => (inFrame ? null : asQuery(request)));
	const authorize = createAuthorizeAppConnectMutation();
	let refused = $state<unknown>(undefined);
	let handedBack = $state<string | null>(null);

	interface Problem {
		error: string;
		backToApp: string | null;
	}

	function describe(cause: unknown): Problem {
		if (cause instanceof ApiError && cause.status === 404) {
			return { error: 'Signing in apps is not turned on for this server.', backToApp: null };
		}
		if (cause instanceof ApiError && cause.status < 500) {
			const details = cause.details as { redirect?: unknown } | null;
			return { error: cause.message, backToApp: appAddress(details?.redirect) };
		}
		return {
			error:
				cause instanceof ApiError
					? 'The server could not answer. Try again from the app.'
					: 'Could not reach the server.',
			backToApp: null
		};
	}

	const refusal = $derived(refused === undefined ? null : describe(refused));

	// Both buttons leave for the app within the click, which is what lets
	// the browser hand an app address over.
	async function allow() {
		try {
			const answer = await authorize.mutateAsync(request);
			const target = appAddress(answer.redirect);
			if (!target) throw new Error('not an app address');
			handedBack = target;
			handBack(target);
		} catch (cause) {
			refused = cause;
		}
	}

	function cancel(address: string) {
		const target = appAddress(address);
		if (!target) return;
		handedBack = target;
		handBack(target);
	}
</script>

<svelte:head>
	<title>Connect an app - DroppedNeedle</title>
</svelte:head>

{#snippet problem(found: Problem)}
	<p class="connect-line" role="alert">{found.error}</p>
	{#if found.backToApp}
		<Button href={found.backToApp} variant="paper" size="sm">Back to the app</Button>
	{/if}
{/snippet}

<AuthFrame kicker="Connect an app">
	<AuthCard icon={KeyRound} centred>
		{#if handedBack}
			<p class="connect-line">Returning to the app.</p>
			<Button href={handedBack} variant="primary" size="display" class="w-full">Open the app</Button
			>
		{:else if inFrame}
			{@render problem({
				error: 'This page cannot be used inside another page.',
				backToApp: null
			})}
		{:else if refusal}
			{@render problem(refusal)}
		{:else}
			<QueryBoundary query={requestQuery} what="the app's request">
				{#snippet failed(cause)}
					{@render problem(describe(cause))}
				{/snippet}
				{#snippet children(view)}
					<h2 class="connect-title">Connect {view.client_name}?</h2>
					<p class="connect-line">
						<strong>{view.client_name}</strong> wants to use DroppedNeedle as
						<strong>{signedInAs}</strong>.
					</p>
					<dl class="connect-facts">
						{#if view.device_name}
							<dt>Device</dt>
							<dd>{view.device_name}</dd>
						{/if}
						<dt>App-password</dt>
						<dd>{view.password_name}</dd>
					</dl>
					<p class="connect-line connect-line--small">
						Revoke it any time under Profile, Connect Apps.
						{#if view.replaces_existing}
							It replaces your app-password of that name.
						{/if}
					</p>
					<p class="connect-warn" role="note">
						<TriangleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
						<span>Only allow this if you just tapped "Sign in" in this app yourself.</span>
					</p>
					{#if view.at_limit}
						<p class="text-sm font-semibold text-error" role="alert">
							You have the most app-passwords allowed. Revoke one under Profile, Connect Apps first.
						</p>
					{/if}
					<div class="flex w-full gap-2">
						<Button
							variant="paper"
							size="display"
							class="flex-1"
							onclick={() => cancel(view.cancel_redirect)}
							disabled={authorize.isPending}>Cancel</Button
						>
						<Button
							variant="primary"
							size="display"
							class="flex-1"
							onclick={allow}
							loading={authorize.isPending}
							disabled={authorize.isPending || view.at_limit}>Allow</Button
						>
					</div>
				{/snippet}
			</QueryBoundary>
		{/if}
	</AuthCard>
</AuthFrame>

<style>
	.connect-title {
		font-family: var(--font-display);
		font-size: 1.75rem;
		font-weight: 400;
		line-height: 1;
		letter-spacing: 0.02em;
		text-transform: uppercase;
		overflow-wrap: anywhere;
	}
	.connect-line {
		font-size: 0.9375rem;
		line-height: 1.5;
		color: var(--dn-muted);
	}
	.connect-line strong {
		color: var(--dn-paper-content);
	}
	.connect-line--small {
		font-size: 0.8125rem;
	}
	.connect-facts {
		display: grid;
		grid-template-columns: auto 1fr;
		gap: 0.375rem 1rem;
		width: 100%;
		padding: 0.875rem 1rem;
		border: 1.5px solid var(--dn-edge);
		border-radius: 0.5rem;
		text-align: left;
		font-size: 0.875rem;
	}
	.connect-facts dt {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.14em;
		line-height: 1.6rem;
		text-transform: uppercase;
		color: var(--dn-muted);
	}
	.connect-facts dd {
		overflow-wrap: anywhere;
		font-weight: 600;
		line-height: 1.6rem;
	}
	.connect-warn {
		display: flex;
		align-items: flex-start;
		gap: 0.5rem;
		text-align: left;
		font-size: 0.875rem;
		font-weight: 600;
		color: var(--color-warning);
	}
</style>
