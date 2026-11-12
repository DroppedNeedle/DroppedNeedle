<script lang="ts">
	import '../../auth.css';
	import { ApiError } from '$lib/api/errors';
	import { withBasePath } from '$lib/utils/basePath';
	import { createPasswordRecoveryResetMutation } from '$lib/features/auth/queries/AuthMutations.svelte';
	import {
		ArrowLeft,
		Check,
		ChevronDown,
		Copy,
		Eye,
		EyeOff,
		KeyRound,
		ShieldCheck
	} from 'lucide-svelte';
	import AuthCard from '$lib/ui/AuthCard.svelte';
	import AuthFrame from '$lib/ui/AuthFrame.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Field from '$lib/ui/Field.svelte';
	import Input from '$lib/ui/Input.svelte';
	import TextField from '$lib/ui/TextField.svelte';

	const resetPassword = createPasswordRecoveryResetMutation();

	let username = $state('');
	let recoveryCode = $state('');
	let newPassword = $state('');
	let confirmPassword = $state('');
	let showPasswords = $state(false);
	let error = $state<string | null>(null);
	let complete = $state(false);
	let copied = $state(false);

	const commandUsername = $derived(
		/^[a-zA-Z0-9._-]{3,32}$/.test(username.trim()) ? username.trim() : 'YOUR_USERNAME'
	);
	const commandReady = $derived(commandUsername !== 'YOUR_USERNAME');
	const toolCommand = 'docker compose run --rm droppedneedle droppedneedle-tool recovery-code';
	const command = $derived(
		`${toolCommand} ${commandUsername.startsWith('-') ? '-- ' : ''}${commandUsername}`
	);

	async function handleReset() {
		error = null;
		if (newPassword !== confirmPassword) {
			error = 'Passwords do not match';
			return;
		}
		if (newPassword.length < 12) {
			error = 'Password must be at least 12 characters';
			return;
		}
		if (new TextEncoder().encode(newPassword).length > 512) {
			error = 'Password is too long. Use 512 bytes or fewer.';
			return;
		}
		try {
			await resetPassword.mutateAsync({
				username,
				recovery_code: recoveryCode,
				new_password: newPassword
			});
			complete = true;
			newPassword = '';
			confirmPassword = '';
			recoveryCode = '';
		} catch (cause) {
			error = cause instanceof ApiError ? cause.message : 'Could not reset password';
		} finally {
			resetPassword.reset();
		}
	}

	async function copyCommand() {
		if (!commandReady) return;
		try {
			await navigator.clipboard.writeText(command);
			copied = true;
			setTimeout(() => (copied = false), 2000);
		} catch {
			copied = false;
		}
	}
</script>

<svelte:head>
	<title>Recover account - DroppedNeedle</title>
</svelte:head>

<AuthFrame kicker="Account recovery" width="md">
	{#if complete}
		<div aria-live="polite">
			<AuthCard icon={Check} title="Password changed" centred>
				<p class="recovery-line">
					All other DroppedNeedle browser sessions have been signed out. Sign in with your new
					password.
				</p>
				<p class="recovery-line recovery-line--small">
					Connect Apps passwords were not changed. Review them after signing in if you suspect that
					someone accessed your account.
				</p>
				{#snippet actions()}
					<Button
						href={withBasePath('/login?local')}
						variant="primary"
						size="display"
						class="w-full"
					>
						Return to sign in
					</Button>
				{/snippet}
			</AuthCard>
		</div>
	{:else}
		<AuthCard
			icon={KeyRound}
			title="Recover your local account"
			lead="Enter the one-time code from your DroppedNeedle administrator."
			{error}
		>
			<form
				class="flex flex-col gap-5"
				onsubmit={(event) => {
					event.preventDefault();
					void handleReset();
				}}
			>
				<TextField
					label="Username"
					bind:value={username}
					autocomplete="username"
					required
					placeholder="Your DroppedNeedle username"
				/>

				<Field label="Recovery code" help="Codes expire after 15 minutes.">
					{#snippet children(control)}
						<Input
							{...control}
							label="Recovery code"
							mono
							class="w-full uppercase tracking-[0.08em]"
							bind:value={recoveryCode}
							autocomplete="one-time-code"
							autocapitalize="characters"
							spellcheck="false"
							required
							placeholder="XXXX-XXXX-XXXX-XXXX-XXXX"
						/>
					{/snippet}
				</Field>

				<div class="grid gap-5 sm:grid-cols-2 sm:gap-4">
					<Field label="New password">
						{#snippet children(control)}
							<Input
								{...control}
								label="New password"
								type={showPasswords ? 'text' : 'password'}
								class="w-full"
								bind:value={newPassword}
								autocomplete="new-password"
								minlength={12}
								required
							/>
						{/snippet}
					</Field>
					<Field label="Confirm password">
						{#snippet children(control)}
							<Input
								{...control}
								label="Confirm password"
								type={showPasswords ? 'text' : 'password'}
								class="w-full"
								bind:value={confirmPassword}
								autocomplete="new-password"
								minlength={12}
								required
							/>
						{/snippet}
					</Field>
				</div>

				<label class="recovery-show">
					<input type="checkbox" class="checkbox checkbox-sm" bind:checked={showPasswords} />
					{#if showPasswords}<EyeOff class="size-4" aria-hidden="true" />{:else}<Eye
							class="size-4"
							aria-hidden="true"
						/>{/if}
					Show passwords
				</label>

				<Button
					type="submit"
					variant="primary"
					size="display"
					class="w-full"
					loading={resetPassword.isPending}
				>
					Reset password
				</Button>
			</form>
		</AuthCard>

		<details class="owner-help">
			<summary class="owner-help__bar">
				<ShieldCheck class="size-5 shrink-0 text-title" aria-hidden="true" />
				<span class="flex-1">Recover as server owner</span>
				<ChevronDown class="owner-help__chevron size-5 shrink-0" aria-hidden="true" />
			</summary>
			<div class="owner-help__body">
				<p>
					If no other administrator can help, use the machine running DroppedNeedle to create a
					recovery code. Only someone with host access can run this command.
				</p>
				<p>
					Stop the server first (<code>docker compose stop droppedneedle</code>): the command
					refuses to run while it is up. Then run this from the folder with your compose file.
				</p>
				<div class="owner-help__command">
					<code>{command}</code>
					<Button
						variant="outline"
						size="xs"
						class="shrink-0"
						icon={copied ? Check : Copy}
						onclick={() => void copyCommand()}
						disabled={!commandReady}
						aria-label="Copy recovery command"
					>
						{copied ? 'Copied' : 'Copy'}
					</Button>
				</div>
				{#if !commandReady}
					<p class="owner-help__hint">Enter your username above to copy a ready-to-run command.</p>
				{/if}
				<p>
					It prints a code that works once, for 15 minutes. Start the server again (<code
						>docker compose up -d droppedneedle</code
					>) and enter the username, the code and a new password above. An account that only signs
					in through single sign-on, Plex or Jellyfin gets its first password this way and keeps its
					other sign-in.
				</p>
				<p>
					The username is the DroppedNeedle one, which can differ from the name you have with a
					single sign-on provider. To see the admin usernames, run
					<code>{toolCommand} <span class="whitespace-nowrap">--list-admins</span></code>.
					<code>droppedneedle</code> in these commands is the service name in your compose file; use yours
					if it differs.
				</p>
			</div>
		</details>
	{/if}
	<div class="recovery-back">
		<Button href={withBasePath('/login?local')} variant="caps" size="sm" icon={ArrowLeft}>
			Back to sign in
		</Button>
	</div>
</AuthFrame>

<style>
	.recovery-line {
		max-width: 24rem;
		font-size: 0.9375rem;
		line-height: 1.5;
		color: var(--dn-muted);
	}
	.recovery-line--small {
		font-size: 0.8125rem;
	}

	.recovery-show {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		width: fit-content;
		font-size: 0.875rem;
		font-weight: 600;
		cursor: pointer;
	}

	/* The owner help: a dark bar under the paper that opens downward. */
	.owner-help {
		margin-top: 1.5rem;
		overflow: hidden;
		border: 1px solid var(--dn-line);
		border-radius: 0.625rem;
		background: var(--color-base-200);
	}
	.owner-help__bar {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		padding: 0.95rem 1.125rem;
		list-style: none;
		font-size: 0.8125rem;
		font-weight: 800;
		letter-spacing: 0.12em;
		text-transform: uppercase;
		cursor: pointer;
	}
	.owner-help__bar::-webkit-details-marker {
		display: none;
	}
	.owner-help__bar:hover {
		background: var(--color-neutral);
	}
	.owner-help__bar:focus-visible {
		outline-offset: -3px;
	}
	.owner-help[open] .owner-help__bar :global(.owner-help__chevron) {
		rotate: 180deg;
	}
	.owner-help__body {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		padding: 1rem 1.125rem 1.25rem;
		border-top: 1px solid var(--dn-line);
		font-size: 0.8125rem;
		line-height: 1.55;
		color: var(--color-muted);
	}
	.owner-help__body code {
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: var(--color-base-content);
	}
	/* the command wraps at its spaces, never inside a word */
	.owner-help__command {
		display: flex;
		align-items: flex-start;
		gap: 0.75rem;
		padding: 0.875rem 1rem;
		border: 1px solid var(--dn-line);
		border-radius: 0.5rem;
		background: var(--color-base-100);
	}
	.owner-help__command code {
		flex: 1;
		min-width: 0;
		font-size: 0.8125rem;
		line-height: 1.6;
		overflow-wrap: anywhere;
		color: var(--dn-title);
	}
	.owner-help__hint {
		font-style: italic;
	}
	@media (max-width: 639px) {
		.owner-help__body {
			padding-inline: 0.875rem;
		}
		.owner-help__command {
			padding: 0.75rem;
		}
		.owner-help__command code {
			font-size: 0.75rem;
		}
	}

	.recovery-back {
		display: flex;
		justify-content: center;
		margin-top: 1.5rem;
	}
</style>
