<script lang="ts">
	import { AtSign, Check, KeyRound, Mail, Pencil, UserCog } from 'lucide-svelte';
	import { ApiError } from '$lib/api/client';
	import {
		createChangePasswordMutation,
		createSetPasswordMutation,
		createUpdateEmailMutation,
		createUpdateUsernameMutation
	} from '$lib/queries/profile/ProfileMutations.svelte';
	import type { ProfileData } from '$lib/queries/profile/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';
	import InlineEditField from './InlineEditField.svelte';

	interface Props {
		profile: ProfileData;
		hasLocalPassword: boolean;
	}

	let { profile, hasLocalPassword }: Props = $props();

	const userId = authStore.user?.id ?? '';

	const usernameMutation = createUpdateUsernameMutation(userId);
	const emailMutation = createUpdateEmailMutation(userId);
	const changePasswordMutation = createChangePasswordMutation(userId);
	const setPasswordMutation = createSetPasswordMutation(userId);

	let editingUsername = $state(false);
	let editingEmail = $state(false);

	let showPasswordForm = $state(false);
	let currentPassword = $state('');
	let newPassword = $state('');
	let passwordError = $state<string | null>(null);
	let passwordDone = $state(false);

	const passwordPending = $derived(
		changePasswordMutation.isPending || setPasswordMutation.isPending
	);

	function togglePasswordForm() {
		showPasswordForm = !showPasswordForm;
		currentPassword = '';
		newPassword = '';
		passwordError = null;
		passwordDone = false;
	}

	async function submitPassword() {
		passwordError = null;
		try {
			if (hasLocalPassword) {
				await changePasswordMutation.mutateAsync({
					current_password: currentPassword,
					new_password: newPassword
				});
			} else {
				await setPasswordMutation.mutateAsync({ new_password: newPassword });
			}
			currentPassword = '';
			newPassword = '';
			showPasswordForm = false;
			passwordDone = true;
		} catch (e) {
			passwordError = e instanceof ApiError ? e.message : 'Could not reach the server';
		}
	}
</script>

<section id="account" class="scroll-mt-24 xl:ml-40">
	<h2
		class="mb-4 flex items-center gap-2 text-sm font-semibold uppercase tracking-widest text-base-content/50"
	>
		<UserCog class="h-4 w-4" />
		Account
	</h2>
	<Card variant="bordered" padding="none" class="overflow-hidden">
		<div class="divide-y divide-base-300/30">
			<div class="flex items-start gap-3 px-5 py-4">
				<div
					class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-base-300/60 text-base-content/70"
				>
					<AtSign class="h-4 w-4" />
				</div>
				<div class="min-w-0 flex-1">
					<p class="text-[10px] font-medium uppercase tracking-wider text-base-content/40">
						Username
					</p>
					{#if editingUsername}
						<InlineEditField
							label="Username"
							value={profile.username_display ?? profile.username ?? ''}
							placeholder="username"
							autocomplete="username"
							pending={usernameMutation.isPending}
							onsave={(username) => usernameMutation.mutateAsync({ username })}
							ondone={() => (editingUsername = false)}
							class="mt-1"
						/>
					{:else}
						<p class="truncate text-sm font-medium">
							@{profile.username_display ?? profile.username ?? '-'}
						</p>
					{/if}
				</div>
				{#if !editingUsername}
					<IconButton
						label="Edit username"
						icon={Pencil}
						size="sm"
						onclick={() => (editingUsername = true)}
					/>
				{/if}
			</div>

			<div class="flex items-start gap-3 px-5 py-4">
				<div
					class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-base-300/60 text-base-content/70"
				>
					<Mail class="h-4 w-4" />
				</div>
				<div class="min-w-0 flex-1">
					<p class="text-[10px] font-medium uppercase tracking-wider text-base-content/40">Email</p>
					{#if editingEmail}
						<InlineEditField
							label="Email"
							type="email"
							value={profile.email ?? ''}
							placeholder="name@example.com (optional)"
							autocomplete="email"
							pending={emailMutation.isPending}
							onsave={(email) => emailMutation.mutateAsync({ email: email.trim() || null })}
							ondone={() => (editingEmail = false)}
							class="mt-1"
						/>
					{:else if profile.email}
						<p class="truncate text-sm font-medium">{profile.email}</p>
					{:else}
						<p class="text-sm text-base-content/40">Not set</p>
					{/if}
				</div>
				{#if !editingEmail}
					<IconButton
						label="Edit email"
						icon={Pencil}
						size="sm"
						onclick={() => (editingEmail = true)}
					/>
				{/if}
			</div>

			<div class="px-5 py-4">
				<div class="flex items-center gap-3">
					<div
						class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-base-300/60 text-base-content/70"
					>
						<KeyRound class="h-4 w-4" />
					</div>
					<div class="min-w-0 flex-1">
						<p class="text-[10px] font-medium uppercase tracking-wider text-base-content/40">
							Password
						</p>
						{#if hasLocalPassword}
							<p class="text-sm font-medium tracking-widest">••••••••</p>
						{:else}
							<p class="text-sm text-base-content/40">No local password</p>
						{/if}
					</div>
					<Button
						variant="ghost"
						size="sm"
						icon={showPasswordForm ? undefined : hasLocalPassword ? Pencil : KeyRound}
						onclick={togglePasswordForm}
					>
						{#if showPasswordForm}
							Cancel
						{:else if hasLocalPassword}
							Change
						{:else}
							Set a password
						{/if}
					</Button>
				</div>

				{#if passwordDone && !showPasswordForm}
					<p class="mt-2 flex items-center gap-1.5 pl-12 text-xs text-success">
						<Check class="h-3.5 w-3.5" />
						{hasLocalPassword ? 'Password updated' : 'Local password set'}
					</p>
				{/if}

				{#if showPasswordForm}
					<div class="mt-3 space-y-2 pl-0 sm:pl-12">
						{#if hasLocalPassword}
							<Input
								type="password"
								label="Current password"
								size="sm"
								class="w-full max-w-sm"
								autocomplete="current-password"
								placeholder="Current password"
								bind:value={currentPassword}
							/>
						{:else}
							<p class="text-xs text-base-content/50">
								Add a password so you can also sign in with your username.
							</p>
						{/if}
						<Input
							type="password"
							label="New password"
							size="sm"
							class="w-full max-w-sm"
							autocomplete="new-password"
							placeholder="New password (min 12 characters)"
							bind:value={newPassword}
						/>
						{#if passwordError}
							<p class="text-xs text-error">{passwordError}</p>
						{/if}
						<div class="flex gap-2 pt-1">
							<Button
								variant="primary"
								size="sm"
								pill
								loading={passwordPending}
								disabled={!newPassword || (hasLocalPassword && !currentPassword)}
								onclick={() => void submitPassword()}
							>
								{hasLocalPassword ? 'Update password' : 'Set password'}
							</Button>
							<Button variant="ghost" size="sm" onclick={togglePasswordForm}>Cancel</Button>
						</div>
					</div>
				{/if}
			</div>
		</div>
	</Card>
</section>
