<script lang="ts">
	import { ApiError } from '$lib/api/client';
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

	type OwnerTab = 'compose' | 'docker' | 'source';

	const resetPassword = createPasswordRecoveryResetMutation();

	let username = $state('');
	let recoveryCode = $state('');
	let newPassword = $state('');
	let confirmPassword = $state('');
	let showPasswords = $state(false);
	let error = $state<string | null>(null);
	let complete = $state(false);
	let ownerTab = $state<OwnerTab>('compose');
	let copied = $state(false);

	const commandUsername = $derived(
		/^[a-zA-Z0-9._-]{3,32}$/.test(username.trim()) ? username.trim() : 'YOUR_USERNAME'
	);
	const commandReady = $derived(commandUsername !== 'YOUR_USERNAME');
	const commands = $derived<Record<OwnerTab, string>>({
		compose: `docker compose exec --user droppedneedle droppedneedle python -m droppedneedle_cli recovery-code ${commandUsername}`,
		docker: `docker exec --user droppedneedle droppedneedle python -m droppedneedle_cli recovery-code ${commandUsername}`,
		source: `cd backend && .venv/bin/python -m droppedneedle_cli recovery-code ${commandUsername}`
	});

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
		if (new TextEncoder().encode(newPassword).length > 72) {
			error = 'Password is too long. Use 72 UTF-8 bytes or fewer.';
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
			await navigator.clipboard.writeText(commands[ownerTab]);
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
			<img src="/logo_icon.png" alt="" aria-hidden="true" class="recovery-mark" />
			<h1 class="recovery-wordmark">DroppedNeedle</h1>
			<div class="recovery-rule" aria-hidden="true"></div>
			<p class="recovery-kicker">Account recovery</p>
		</div>

		<div class="overflow-hidden rounded-box border border-base-300 bg-base-200 shadow-xl">
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
					<a href="/login" class="btn btn-primary mt-7 w-full sm:w-auto">Return to sign in</a>
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
					<fieldset class="fieldset">
						<legend class="fieldset-legend">Username</legend>
						<input
							type="text"
							class="input input-bordered w-full"
							bind:value={username}
							aria-label="Username"
							autocomplete="username"
							required
							placeholder="Your DroppedNeedle username"
						/>
					</fieldset>

					<fieldset class="fieldset">
						<legend class="fieldset-legend">Recovery code</legend>
						<input
							type="text"
							class="input input-bordered recovery-code w-full"
							bind:value={recoveryCode}
							aria-label="Recovery code"
							autocomplete="one-time-code"
							autocapitalize="characters"
							spellcheck="false"
							required
							placeholder="XXXX-XXXX-XXXX-XXXX-XXXX"
						/>
						<p class="mt-1 text-xs text-base-content/45">Codes expire after 15 minutes.</p>
					</fieldset>

					<div class="grid gap-4 sm:grid-cols-2">
						<fieldset class="fieldset">
							<legend class="fieldset-legend">New password</legend>
							<input
								type={showPasswords ? 'text' : 'password'}
								class="input input-bordered w-full"
								bind:value={newPassword}
								aria-label="New password"
								autocomplete="new-password"
								minlength="12"
								required
							/>
						</fieldset>
						<fieldset class="fieldset">
							<legend class="fieldset-legend">Confirm password</legend>
							<input
								type={showPasswords ? 'text' : 'password'}
								class="input input-bordered w-full"
								bind:value={confirmPassword}
								aria-label="Confirm password"
								autocomplete="new-password"
								minlength="12"
								required
							/>
						</fieldset>
					</div>

					<label class="flex cursor-pointer items-center gap-2 text-xs text-base-content/55">
						<input type="checkbox" class="checkbox checkbox-xs" bind:checked={showPasswords} />
						{#if showPasswords}<EyeOff class="h-3.5 w-3.5" />{:else}<Eye class="h-3.5 w-3.5" />{/if}
						Show passwords
					</label>

					{#if error}
						<div class="alert alert-error py-2 text-sm" role="alert">{error}</div>
					{/if}

					<button class="btn btn-primary w-full" type="submit" disabled={resetPassword.isPending}>
						{#if resetPassword.isPending}
							<span class="loading loading-spinner loading-sm"></span>
						{/if}
						Reset password
					</button>
				</form>
			{/if}
		</div>

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
					<div class="mt-3 flex flex-wrap gap-1" role="tablist" aria-label="Installation method">
						{#each ['compose', 'docker', 'source'] as tab (tab)}
							<button
								type="button"
								role="tab"
								aria-selected={ownerTab === tab}
								class="btn btn-xs {ownerTab === tab ? 'btn-primary' : 'btn-ghost'}"
								onclick={() => (ownerTab = tab as OwnerTab)}
							>
								{tab === 'compose' ? 'Docker Compose' : tab === 'docker' ? 'Docker' : 'Source'}
							</button>
						{/each}
					</div>
					<div class="mt-3 flex items-start gap-2 rounded-box bg-base-300/70 p-3">
						<code class="min-w-0 flex-1 break-all font-mono text-xs leading-relaxed"
							>{commands[ownerTab]}</code
						>
						<button
							type="button"
							class="btn btn-ghost btn-xs shrink-0 gap-1"
							onclick={() => void copyCommand()}
							disabled={!commandReady}
							aria-label="Copy recovery command"
						>
							{#if copied}<Check class="h-3.5 w-3.5" /> Copied{:else}<Copy class="h-3.5 w-3.5" /> Copy{/if}
						</button>
					</div>
					{#if !commandReady}
						<p class="mt-2 text-xs text-base-content/50">
							Enter your username above to copy a ready-to-run command.
						</p>
					{/if}
				</div>
			</details>
		{/if}

		<a
			href="/login"
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

	.recovery-code {
		font-family: var(--font-mono);
		letter-spacing: 0.08em;
		text-transform: uppercase;
	}

	.owner-help[open] {
		background: oklch(from var(--color-base-200) l c h / 0.95);
	}
</style>
