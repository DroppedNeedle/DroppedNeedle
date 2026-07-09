<script lang="ts">
	import { page } from '$app/state';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { ApiError } from '$lib/api/client';
	import { createOidcExchangeMutation } from '$lib/queries/auth/AuthMutations.svelte';
	import { toAuthUser } from '$lib/queries/auth/types';
	import { onMount } from 'svelte';
	import { Music } from 'lucide-svelte';

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
			// sign-in back to /login
			window.location.assign('/');
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

<div class="min-h-screen bg-base-100 flex items-center justify-center p-4">
	<div class="flex flex-col items-center gap-4 text-center">
		<div class="bg-primary/10 rounded-full p-4">
			<Music class="h-10 w-10 text-primary" />
		</div>

		{#if error}
			<div class="flex flex-col items-center gap-3 max-w-sm">
				<p class="text-base-content/70 text-sm">{error}</p>
				<a href="/login" class="btn btn-primary btn-sm">Back to sign in</a>
			</div>
		{:else}
			<p class="text-base-content/60 text-sm">Completing sign-in…</p>
			<span class="loading loading-spinner loading-md text-primary"></span>
		{/if}
	</div>
</div>
