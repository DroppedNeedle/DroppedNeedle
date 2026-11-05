<script lang="ts">
	import '../../../auth.css';
	import { page } from '$app/state';
	import { withBasePath } from '$lib/utils/basePath';
	import { takeSignInReturn } from '$lib/utils/signInReturn';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { ApiError } from '$lib/api/errors';
	import { createOidcExchangeMutation } from '$lib/features/auth/queries/AuthMutations.svelte';
	import { toAuthUser } from '$lib/features/auth/queries/types';
	import { onMount } from 'svelte';
	import AuthCard from '$lib/ui/AuthCard.svelte';
	import AuthFrame from '$lib/ui/AuthFrame.svelte';
	import Button from '$lib/ui/Button.svelte';

	let error = $state<string | null>(null);

	const oidcExchange = createOidcExchangeMutation();

	onMount(async () => {
		const code = page.url.searchParams.get('code');
		if (!code) {
			error = 'Missing authentication code. Please try signing in again.';
			return;
		}
		try {
			const data = await oidcExchange.mutateAsync({ code });
			authStore.setUser(toAuthUser(data.user));
			// hard navigation on purpose: the exchange call just set the session
			// cookie, and a soft goto() can outrun it and bounce the first
			// sign-in back to /login; it lands where sign-in was asked from
			window.location.assign(withBasePath(takeSignInReturn()));
		} catch (e) {
			error =
				e instanceof ApiError
					? 'Authentication failed. The link may have expired, please try again.'
					: 'Could not reach the server.';
		}
	});
</script>

<svelte:head>
	<title>Signing in - DroppedNeedle</title>
</svelte:head>

<AuthFrame kicker={error ? 'Sign in to continue' : 'Signing you in…'}>
	{#if error}
		<AuthCard {error}>
			<Button href={withBasePath('/login')} variant="primary" size="display" class="w-full">
				Back to sign in
			</Button>
		</AuthCard>
	{:else}
		<div class="callback-wait" aria-busy="true">
			<span class="loading loading-spinner motion-keep callback-wait__ring" aria-hidden="true"
			></span>
			<p>Completing sign-in…</p>
		</div>
	{/if}
</AuthFrame>

<style>
	.callback-wait {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 1rem;
		color: var(--color-muted);
		font-size: 0.9375rem;
	}
	.callback-wait__ring {
		width: 2.75rem;
		color: var(--dn-title);
	}
</style>
