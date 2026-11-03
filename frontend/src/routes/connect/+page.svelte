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
	} from '$lib/queries/connect-apps.svelte';
	import type { ConnectRequest } from '$lib/queries/connect-apps.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { appAddress, framed, handBack } from '$lib/utils/appHandBack';
	import { KeyRound, TriangleAlert } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
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
	<p class="text-sm text-base-content/70" role="alert">{found.error}</p>
	{#if found.backToApp}
		<Button href={found.backToApp} variant="outline" size="sm">Back to the app</Button>
	{/if}
{/snippet}

<div class="flex min-h-screen items-center justify-center bg-base-100 p-4">
	<Card variant="raised" class="w-full max-w-md">
		<div class="flex flex-col items-center gap-4 text-center">
			<div class="rounded-full bg-primary/10 p-4">
				<KeyRound class="h-10 w-10 text-primary" aria-hidden="true" />
			</div>

			{#if handedBack}
				<p class="text-sm text-base-content/70">Returning to the app.</p>
				<Button href={handedBack} variant="primary" size="sm">Open the app</Button>
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
						<h1 class="text-xl font-semibold">Connect {view.client_name}?</h1>
						<p class="text-sm text-base-content/80">
							<strong>{view.client_name}</strong> wants to use DroppedNeedle as
							<strong>{signedInAs}</strong>.
						</p>
						<dl class="w-full text-left text-sm">
							{#if view.device_name}
								<dt class="text-base-content/60">Device</dt>
								<dd class="mb-2 break-words">{view.device_name}</dd>
							{/if}
							<dt class="text-base-content/60">App-password</dt>
							<dd class="break-words">{view.password_name}</dd>
						</dl>
						<p class="text-xs text-base-content/60">
							Revoke it any time under Profile, Connect Apps.
							{#if view.replaces_existing}
								It replaces your app-password of that name.
							{/if}
						</p>
						<p class="flex items-start gap-2 text-left text-sm text-warning" role="note">
							<TriangleAlert class="mt-0.5 h-4 w-4 shrink-0" aria-hidden="true" />
							<span>Only allow this if you just tapped "Sign in" in this app yourself.</span>
						</p>
						{#if view.at_limit}
							<p class="text-sm text-error" role="alert">
								You have the most app-passwords allowed. Revoke one under Profile, Connect Apps
								first.
							</p>
						{/if}
						<div class="flex w-full gap-2">
							<Button
								variant="outline"
								class="flex-1"
								onclick={() => cancel(view.cancel_redirect)}
								disabled={authorize.isPending}>Cancel</Button
							>
							<Button
								variant="primary"
								class="flex-1"
								onclick={allow}
								loading={authorize.isPending}
								disabled={authorize.isPending || view.at_limit}>Allow</Button
							>
						</div>
					{/snippet}
				</QueryBoundary>
			{/if}
		</div>
	</Card>
</div>
