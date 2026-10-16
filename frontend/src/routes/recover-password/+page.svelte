<script lang="ts">
	import '../../auth.css';
	import { ApiError } from '$lib/api/client';
	import { withBasePath } from '$lib/utils/basePath';
	import { createPasswordRecoveryResetMutation } from '$lib/queries/auth/AuthMutations.svelte';
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
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
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

<div class="recovery-wrap grain min-h-screen flex items-center justify-center p-4 py-10">
	<div class="w-full max-w-lg">
		<div class="recovery-brand">
			<img
				src={withBasePath('/logo_icon.png')}
				alt=""
				aria-hidden="true"
				class="dn-logo recovery-mark"
			/>
			<h1 class="recovery-wordmark">DroppedNeedle</h1>
			<div class="recovery-rule" aria-hidden="true"></div>
			<p class="recovery-kicker">Account recovery</p>
		</div>

		<Card variant="bordered" padding="none" class="overflow-hidden">
			<div>
				{#if complete}
					<div class="p-7 text-center sm:p-9" aria-live="polite">
						<div
							class="mx-auto mb-5 flex h-14 w-14 items-center justify-center rounded-full bg-success/10"
						>
							<Check class="h-7 w-7 text-success" aria-hidden="true" />
						</div>
						<h2 class="font-display text-2xl font-bold">Password changed</h2>
						<p class="mx-auto mt-2 max-w-sm text-sm leading-relaxed text-base-content/60">
							All other DroppedNeedle browser sessions have been signed out. Sign in with your new
							password.
						</p>
						<p class="mx-auto mt-3 max-w-sm text-xs leading-relaxed text-base-content/45">
							Connect Apps passwords were not changed. Review them after signing in if you suspect
							that someone accessed your account.
						</p>
						<Button href={withBasePath('/login')} variant="primary" class="mt-7 w-full sm:w-auto">
							Return to sign in
						</Button>
					</div>
				{:else}
					<div class="border-b border-base-300 p-6 sm:p-7">
						<div class="flex items-start gap-4">
							<div
								class="flex h-11 w-11 shrink-0 items-center justify-center rounded-box bg-primary/10"
							>
								<KeyRound class="h-5 w-5 text-primary" aria-hidden="true" />
							</div>
							<div>
								<h2 class="font-display text-xl font-bold">Recover your local account</h2>
								<p class="mt-1 text-sm leading-relaxed text-base-content/60">
									Enter the one-time code from your DroppedNeedle administrator.
								</p>
							</div>
						</div>
					</div>

					<form
						class="space-y-4 p-6 sm:p-7"
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

						<div class="grid gap-4 sm:grid-cols-2">
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

						<label class="flex cursor-pointer items-center gap-2 text-xs text-base-content/55">
							<input type="checkbox" class="checkbox checkbox-sm" bind:checked={showPasswords} />
							{#if showPasswords}<EyeOff class="h-3.5 w-3.5" />{:else}<Eye
									class="h-3.5 w-3.5"
								/>{/if}
							Show passwords
						</label>

						{#if error}
							<div class="alert alert-error py-2 text-sm" role="alert">{error}</div>
						{/if}

						<Button
							type="submit"
							variant="primary"
							class="w-full"
							loading={resetPassword.isPending}
						>
							Reset password
						</Button>
					</form>
				{/if}
			</div>
		</Card>

		{#if !complete}
			<details
				class="owner-help group mt-4 overflow-hidden rounded-box border border-base-300 bg-base-200/70"
			>
				<summary class="flex cursor-pointer list-none items-center gap-3 p-4 text-sm font-semibold">
					<ShieldCheck class="h-4 w-4 text-accent" aria-hidden="true" />
					<span class="flex-1">Recover as server owner</span>
					<ChevronDown class="h-4 w-4 text-base-content/40 group-open:rotate-180" />
				</summary>
				<div class="border-t border-base-300 px-4 pb-4 pt-3">
					<p class="text-xs leading-relaxed text-base-content/60">
						If no other administrator can help, use the machine running DroppedNeedle to create a
						recovery code. Only someone with host access can run this command.
					</p>
					<p class="mt-2 text-xs leading-relaxed text-base-content/60">
						Stop the server first (<code class="font-mono">docker compose stop droppedneedle</code
						>): the command refuses to run while it is up. Then run this from the folder with your
						compose file.
					</p>
					<div class="mt-3 flex items-start gap-2 rounded-box bg-base-300/70 p-3">
						<code class="min-w-0 flex-1 break-all font-mono text-xs leading-relaxed">{command}</code
						>
						<Button
							variant="ghost"
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
						<p class="mt-2 text-xs text-base-content/50">
							Enter your username above to copy a ready-to-run command.
						</p>
					{/if}
					<p class="mt-2 text-xs leading-relaxed text-base-content/60">
						It prints a code that works once, for 15 minutes. Start the server again (<code
							class="font-mono">docker compose up -d droppedneedle</code
						>) and enter the username, the code and a new password above. An account that only signs
						in through single sign-on, Plex or Jellyfin gets its first password this way and keeps
						its other sign-in.
					</p>
					<p class="mt-2 text-xs leading-relaxed text-base-content/60">
						The username is the DroppedNeedle one, which can differ from the name you have with a
						single sign-on provider. To see the admin usernames, run
						<code class="break-all font-mono">{toolCommand} --list-admins</code>.
						<code class="font-mono">droppedneedle</code> in these commands is the service name in your
						compose file; use yours if it differs.
					</p>
				</div>
			</details>
		{/if}
		<a
			href={withBasePath('/login')}
			class="mx-auto mt-5 flex w-fit items-center gap-2 text-sm text-base-content/50 hover:text-primary"
		>
			<ArrowLeft class="h-4 w-4" aria-hidden="true" /> Back to sign in
		</a>
	</div>
</div>

<style>
	.recovery-wrap {
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

	.recovery-brand {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.45rem;
		margin-bottom: 1.75rem;
	}

	.recovery-mark {
		height: 2.5rem;
		width: auto;
		opacity: 0.9;
	}

	.recovery-wordmark {
		font-family: var(--font-display);
		font-size: clamp(2.4rem, 11vw, 3.5rem);
		font-weight: 800;
		line-height: 0.9;
		letter-spacing: 0.01em;
		color: oklch(from var(--color-base-content) l c h / 0.95);
		text-shadow: 0 2px 1px oklch(from var(--color-base-300) l c h / 0.4);
	}

	.recovery-rule {
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

	.recovery-kicker {
		font-family: var(--font-mono);
		font-size: 0.7rem;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.45);
	}

	.owner-help[open] {
		background: oklch(from var(--color-base-200) l c h / 0.95);
	}
</style>
