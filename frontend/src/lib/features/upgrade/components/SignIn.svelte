<script lang="ts">
	import { ApiError } from '$lib/api/errors';
	import ServerLogHint from '$lib/features/auth/components/ServerLogHint.svelte';
	import { createUpgradeSignInMutation } from '$lib/features/upgrade/queries/UpgradeQueries.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Field from '$lib/ui/Field.svelte';
	import Input from '$lib/ui/Input.svelte';
	import TextField from '$lib/ui/TextField.svelte';

	let useCode = $state(false);
	let username = $state('');
	let password = $state('');
	let code = $state('');
	let error = $state<string | null>(null);

	const signIn = createUpgradeSignInMutation();

	async function submit() {
		error = null;
		try {
			await signIn.mutateAsync(
				useCode ? { method: 'code', code } : { method: 'password', username, password }
			);
		} catch (cause) {
			if (cause instanceof ApiError && cause.status === 429) {
				error = 'Too many attempts. Wait a moment and try again.';
			} else {
				error = cause instanceof ApiError ? cause.message : 'Could not reach the server';
			}
		}
	}

	function swap() {
		useCode = !useCode;
		error = null;
	}
</script>

<Card variant="bordered" title="Sign in to upgrade">
	<form
		class="flex flex-col gap-4"
		onsubmit={(event) => {
			event.preventDefault();
			void submit();
		}}
	>
		{#if useCode}
			<ServerLogHint />
			<TextField label="Code" bind:value={code} mono required autocomplete="one-time-code" />
		{:else}
			<p class="text-sm text-base-content/70">Use a v2 admin account.</p>
			<TextField label="Username" bind:value={username} required autocomplete="username" />
			<Field label="Password">
				{#snippet children(control)}
					<Input
						{...control}
						label="Password"
						type="password"
						class="w-full"
						required
						autocomplete="current-password"
						bind:value={password}
					/>
				{/snippet}
			</Field>
		{/if}

		{#if error}
			<div class="alert alert-error py-2 text-sm" role="alert">{error}</div>
		{/if}

		<Button type="submit" variant="primary" class="w-full" loading={signIn.isPending}>
			Sign in
		</Button>
		<Button variant="ghost" size="sm" class="self-center" onclick={swap}>
			{useCode ? 'Use a password instead' : 'Use a code from the server log'}
		</Button>
	</form>
</Card>
