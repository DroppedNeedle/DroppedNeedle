<script lang="ts">
	import '../../auth.css';
	import { goto } from '$app/navigation';
	import { withBasePath } from '$lib/utils/basePath';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { ApiError } from '$lib/api/errors';
	import { createSetupMutation } from '$lib/features/auth/queries/AuthMutations.svelte';
	import { toAuthUser } from '$lib/features/auth/queries/types';
	import ServerLogHint from '$lib/features/auth/components/ServerLogHint.svelte';
	import { ShieldCheck } from 'lucide-svelte';
	import AuthCard from '$lib/ui/AuthCard.svelte';
	import AuthFrame from '$lib/ui/AuthFrame.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Field from '$lib/ui/Field.svelte';
	import Input from '$lib/ui/Input.svelte';
	import PasswordField from '$lib/ui/PasswordField.svelte';
	import TextField from '$lib/ui/TextField.svelte';

	let displayName = $state('');
	let username = $state('');
	let email = $state('');
	let password = $state('');
	let confirmPassword = $state('');
	let code = $state('');
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
				password,
				code: authStore.codeRequired ? code : undefined
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

<AuthFrame kicker="Welcome to DroppedNeedle">
	<AuthCard
		icon={ShieldCheck}
		title="Admin Account"
		lead="Create your admin account to get started. This only appears once."
		{error}
	>
		<form
			onsubmit={(e) => {
				e.preventDefault();
				void handleSetup();
			}}
			class="flex flex-col gap-5"
		>
			{#if authStore.codeRequired}
				<ServerLogHint />
				<Field label="Code from the server log">
					{#snippet children(control)}
						<Input
							{...control}
							label="Code from the server log"
							class="w-full"
							mono
							required
							autocomplete="one-time-code"
							spellcheck={false}
							bind:value={code}
						/>
					{/snippet}
				</Field>
			{/if}

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

			<PasswordField
				label="Password"
				placeholder="Min. 12 characters"
				required
				autocomplete="new-password"
				bind:value={password}
				bind:shown={showPassword}
			/>

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

			<Button
				type="submit"
				variant="primary"
				size="display"
				class="w-full"
				loading={setup.isPending}
			>
				Create Admin Account
			</Button>
		</form>
	</AuthCard>
</AuthFrame>
