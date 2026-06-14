<script lang="ts">
	import { goto } from '$app/navigation';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { ApiError } from '$lib/api/client';
	import { createSetupMutation } from '$lib/queries/auth/AuthMutations.svelte';
	import { toAuthUser } from '$lib/queries/auth/types';
	import { Music, Eye, EyeOff, ShieldCheck } from 'lucide-svelte';

	let displayName = $state('');
	let email = $state('');
	let password = $state('');
	let confirmPassword = $state('');
	let showPassword = $state(false);
	let error = $state<string | null>(null);

	const setup = createSetupMutation();

	async function handleSetup() {
		error = null;
		if (password !== confirmPassword) {
			error = 'Passwords do not match';
			return;
		}
		if (password.length < 12) {
			error = 'Password must be at least 12 characters';
			return;
		}
		try {
			const data = await setup.mutateAsync({ display_name: displayName, email, password });
			authStore.setUser(toAuthUser(data.user));
			goto('/');
		} catch (e) {
			error =
				e instanceof ApiError ? e.message : 'Could not reach the server. Is Musicseerr running?';
		}
	}
</script>

<svelte:head>
	<title>Setup - Musicseerr</title>
</svelte:head>

<div class="min-h-screen bg-base-100 flex items-center justify-center p-4">
	<div class="w-full max-w-md">
		<div class="flex flex-col items-center mb-8 gap-3">
			<div class="bg-primary/10 rounded-full p-4">
				<Music class="h-10 w-10 text-primary" />
			</div>
			<h1 class="text-3xl font-bold">Welcome to Musicseerr</h1>
			<p class="text-base-content/60 text-sm text-center max-w-xs">
				Create your admin account to get started. This only appears once.
			</p>
		</div>

		<div class="bg-base-200 rounded-box p-6 shadow-lg border border-base-300">
			<div class="flex items-center gap-2 mb-5 pb-4 border-b border-base-300">
				<ShieldCheck class="h-4 w-4 text-accent" />
				<span class="text-sm font-semibold text-base-content/70 uppercase tracking-wider">
					Admin Account
				</span>
			</div>

			<form
				onsubmit={(e) => {
					e.preventDefault();
					void handleSetup();
				}}
				class="flex flex-col gap-4"
			>
				<fieldset class="fieldset">
					<legend class="fieldset-legend">Display Name</legend>
					<input
						type="text"
						class="input input-bordered w-full"
						placeholder="Your name"
						bind:value={displayName}
						required
						autocomplete="name"
					/>
				</fieldset>

				<fieldset class="fieldset">
					<legend class="fieldset-legend">Email</legend>
					<input
						type="email"
						class="input input-bordered w-full"
						placeholder="admin@example.com"
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
								placeholder="Min. 12 characters"
								bind:value={password}
								required
								autocomplete="new-password"
							/>
						{:else}
							<input
								type="password"
								class="grow"
								placeholder="Min. 12 characters"
								bind:value={password}
								required
								autocomplete="new-password"
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

				<fieldset class="fieldset">
					<legend class="fieldset-legend">Confirm Password</legend>
					<input
						type="password"
						class="input input-bordered w-full"
						placeholder="Repeat password"
						bind:value={confirmPassword}
						required
						autocomplete="new-password"
					/>
				</fieldset>

				{#if error}
					<div class="alert alert-error py-2 text-sm">{error}</div>
				{/if}

				<button type="submit" class="btn btn-primary w-full mt-1" disabled={setup.isPending}>
					{#if setup.isPending}
						<span class="loading loading-spinner loading-sm"></span>
					{/if}
					Create Admin Account
				</button>
			</form>
		</div>
	</div>
</div>
