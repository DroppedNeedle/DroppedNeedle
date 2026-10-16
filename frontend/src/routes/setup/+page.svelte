<script lang="ts">
	import '../../auth.css';
	import { goto } from '$app/navigation';
	import { withBasePath } from '$lib/utils/basePath';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { ApiError } from '$lib/api/client';
	import { createSetupMutation } from '$lib/queries/auth/AuthMutations.svelte';
	import { toAuthUser } from '$lib/queries/auth/types';
	import { Music, Eye, EyeOff, ShieldCheck } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Field from '$lib/ui/Field.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import TextField from '$lib/ui/TextField.svelte';

	let displayName = $state('');
	let username = $state('');
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
			const data = await setup.mutateAsync({
				display_name: displayName,
				username,
				email: email || undefined,
				password
			});
			authStore.setUser(toAuthUser(data.user));
			authStore.setSetupRequired(false);
			goto(withBasePath('/'));
		} catch (e) {
			error =
				e instanceof ApiError ? e.message : 'Could not reach the server. Is DroppedNeedle running?';
		}
	}
</script>

<svelte:head>
	<title>Setup - DroppedNeedle</title>
</svelte:head>

<div class="min-h-screen bg-base-100 flex items-center justify-center p-4">
	<div class="w-full max-w-md">
		<div class="flex flex-col items-center mb-8 gap-3">
			<div class="bg-primary/10 rounded-full p-4">
				<Music class="h-10 w-10 text-primary" />
			</div>
			<h1 class="text-3xl font-bold">Welcome to DroppedNeedle</h1>
			<p class="text-base-content/60 text-sm text-center max-w-xs">
				Create your admin account to get started. This only appears once.
			</p>
		</div>

		<Card variant="bordered">
			<div>
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
					<TextField
						label="Display Name"
						placeholder="Your name"
						bind:value={displayName}
						required
						autocomplete="name"
					/>

					<TextField
						label="Username"
						placeholder="jane.smith"
						bind:value={username}
						required
						autocomplete="username"
					/>

					<TextField
						label="Email (optional)"
						type="email"
						placeholder="admin@example.com"
						bind:value={email}
						autocomplete="email"
					/>

					<Field label="Password">
						{#snippet children(control)}
							<div class="join w-full">
								<Input
									{...control}
									label="Password"
									type={showPassword ? 'text' : 'password'}
									class="join-item w-full"
									placeholder="Min. 12 characters"
									required
									autocomplete="new-password"
									bind:value={password}
								/>
								<IconButton
									label="Show password"
									icon={showPassword ? EyeOff : Eye}
									variant="neutral"
									shape="square"
									class="join-item"
									aria-pressed={showPassword}
									onclick={() => (showPassword = !showPassword)}
								/>
							</div>
						{/snippet}
					</Field>

					<Field label="Confirm Password">
						{#snippet children(control)}
							<Input
								{...control}
								label="Confirm Password"
								type="password"
								class="w-full"
								placeholder="Repeat password"
								required
								autocomplete="new-password"
								bind:value={confirmPassword}
							/>
						{/snippet}
					</Field>

					{#if error}
						<div class="alert alert-error py-2 text-sm">{error}</div>
					{/if}

					<Button type="submit" variant="primary" class="w-full mt-1" loading={setup.isPending}>
						Create Admin Account
					</Button>
				</form>
			</div>
		</Card>
	</div>
</div>
