<script lang="ts">
	import { goto } from '$app/navigation';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { ApiError } from '$lib/api/client';
	import { createSetupMutation } from '$lib/queries/auth/AuthMutations.svelte';
	import { toAuthUser } from '$lib/queries/auth/types';
	import { Eye, EyeOff, ShieldCheck } from 'lucide-svelte';

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
			goto('/');
		} catch (e) {
			error =
				e instanceof ApiError ? e.message : 'Could not reach the server. Is DroppedNeedle running?';
		}
	}
</script>

<svelte:head>
	<title>Setup - DroppedNeedle</title>
</svelte:head>

<div class="setup-wrap grain flex min-h-screen items-center justify-center p-4">
	<div class="w-full max-w-md">
		<div class="setup-brand">
			<img src="/logo_icon.png" alt="" aria-hidden="true" class="setup-mark" />
			<p class="setup-eyebrow">Audiophile grade</p>
			<h1 class="setup-wordmark">DroppedNeedle</h1>
			<div class="setup-rule" aria-hidden="true"></div>
			<p class="setup-sub">Create your admin account to get started. This only appears once.</p>
		</div>

		<div
			class="rounded-2xl border border-base-content/8 bg-base-200/50 p-6 backdrop-blur-sm sm:p-7"
		>
			<div
				class="mb-5 flex items-center gap-2.5 border-b border-base-content/8 pb-4 font-mono text-[0.68rem] font-bold uppercase tracking-[0.2em] text-base-content/50"
			>
				<ShieldCheck class="h-4 w-4 text-accent" />
				Admin account
			</div>

			<form
				onsubmit={(e) => {
					e.preventDefault();
					void handleSetup();
				}}
				class="flex flex-col gap-4"
			>
				<div class="flex flex-col gap-1.5">
					<label class="field-label" for="setup-display-name">Display Name</label>
					<input
						id="setup-display-name"
						type="text"
						class="input w-full rounded-full border-base-content/12 bg-base-100/60"
						placeholder="Your name"
						bind:value={displayName}
						required
						autocomplete="name"
					/>
				</div>

				<div class="flex flex-col gap-1.5">
					<label class="field-label" for="setup-username">Username</label>
					<input
						id="setup-username"
						type="text"
						class="input w-full rounded-full border-base-content/12 bg-base-100/60"
						placeholder="jane.smith"
						bind:value={username}
						required
						autocomplete="username"
					/>
				</div>

				<div class="flex flex-col gap-1.5">
					<label class="field-label" for="setup-email">Email (optional)</label>
					<input
						id="setup-email"
						type="email"
						class="input w-full rounded-full border-base-content/12 bg-base-100/60"
						placeholder="admin@example.com"
						bind:value={email}
						autocomplete="email"
					/>
				</div>

				<div class="flex flex-col gap-1.5">
					<span class="field-label">Password</span>
					<label
						class="input flex w-full items-center gap-2 rounded-full border-base-content/12 bg-base-100/60"
					>
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
							class="opacity-50 transition-opacity hover:opacity-100"
							aria-label="Toggle password visibility"
						>
							{#if showPassword}<EyeOff class="h-4 w-4" />{:else}<Eye class="h-4 w-4" />{/if}
						</button>
					</label>
				</div>

				<div class="flex flex-col gap-1.5">
					<label class="field-label" for="setup-confirm-password">Confirm Password</label>
					<input
						id="setup-confirm-password"
						type="password"
						class="input w-full rounded-full border-base-content/12 bg-base-100/60"
						placeholder="Repeat password"
						bind:value={confirmPassword}
						required
						autocomplete="new-password"
					/>
				</div>

				{#if error}
					<div class="alert alert-error rounded-2xl py-2 text-sm">{error}</div>
				{/if}

				<button
					type="submit"
					class="btn btn-primary mt-1 w-full rounded-full"
					disabled={setup.isPending}
				>
					{#if setup.isPending}
						<span class="loading loading-spinner loading-sm"></span>
					{/if}
					Create Admin Account
				</button>
			</form>
		</div>
	</div>
</div>

<style>
	.setup-wrap {
		--grain-opacity: 0.1;
		background:
			radial-gradient(
				circle at 50% -8rem,
				oklch(from var(--color-primary) l c h / 0.08),
				transparent 24rem
			),
			radial-gradient(
				circle at 85% 110%,
				oklch(from var(--color-accent) l c h / 0.05),
				transparent 26rem
			),
			var(--color-base-100);
	}

	.setup-brand {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.5rem;
		margin-bottom: 2rem;
		text-align: center;
	}

	.setup-mark {
		height: 3rem;
		width: auto;
		margin-bottom: 0.25rem;
		opacity: 0.9;
	}

	.setup-eyebrow {
		font-family: var(--font-mono);
		font-size: 0.62rem;
		font-weight: 700;
		letter-spacing: 0.28em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.45);
	}

	.setup-wordmark {
		font-family: var(--font-display);
		font-weight: 800;
		font-size: clamp(2.5rem, 12vw, 3.5rem);
		line-height: 0.85;
		letter-spacing: 0.01em;
		color: oklch(from var(--color-base-content) l c h / 0.95);
		text-shadow: 0 2px 1px rgb(0 0 0 / 0.4);
	}

	.setup-rule {
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

	.setup-sub {
		max-width: 20rem;
		font-size: 0.8rem;
		color: oklch(from var(--color-base-content) l c h / 0.55);
	}

	.field-label {
		margin-left: 1rem;
		font-family: var(--font-mono);
		font-size: 0.62rem;
		font-weight: 700;
		letter-spacing: 0.18em;
		text-transform: uppercase;
		color: oklch(from var(--color-base-content) l c h / 0.5);
	}
</style>
