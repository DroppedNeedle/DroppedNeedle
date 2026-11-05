<script lang="ts">
	import { AtSign, Check, KeyRound, LockKeyhole, Mail, Pencil } from 'lucide-svelte';
	import { ApiError } from '$lib/api/errors';
	import {
		createChangePasswordMutation,
		createSetPasswordMutation,
		createUpdateEmailMutation,
		createUpdateUsernameMutation
	} from '$lib/features/profile/queries/profile.svelte';
	import type { ProfileData } from '$lib/features/profile/queries/profile.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
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

<div class="pf-rows">
	<div class="pf-row">
		<span class="pf-tile" aria-hidden="true"><AtSign class="size-5" /></span>
		<div class="pf-row__main">
			<p class="pf-label">Username</p>
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
				<p class="pf-value font-mono">@{profile.username_display ?? profile.username ?? '-'}</p>
			{/if}
		</div>
		{#if !editingUsername}
			<Button
				variant="outline"
				size="display-sm"
				icon={Pencil}
				class="pf-row__action"
				onclick={() => (editingUsername = true)}
			>
				Edit username
			</Button>
		{/if}
	</div>

	<div class="pf-row">
		<span class="pf-tile" aria-hidden="true"><Mail class="size-5" /></span>
		<div class="pf-row__main">
			<p class="pf-label">Email</p>
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
				<p class="pf-value">{profile.email}</p>
			{:else}
				<p class="pf-value text-muted">Not set</p>
			{/if}
		</div>
		{#if !editingEmail}
			<Button
				variant="outline"
				size="display-sm"
				icon={Pencil}
				class="pf-row__action"
				onclick={() => (editingEmail = true)}
			>
				Edit email
			</Button>
		{/if}
	</div>

	<div class="pf-row pf-row--wrap">
		<span class="pf-tile" aria-hidden="true"><LockKeyhole class="size-5" /></span>
		<div class="pf-row__main">
			<p class="pf-label">Password</p>
			{#if hasLocalPassword}
				<p class="pf-value tracking-[0.3em]">••••••••</p>
			{:else}
				<p class="pf-value text-muted">No local password</p>
			{/if}
		</div>
		<Button
			variant="outline"
			size="display-sm"
			icon={showPasswordForm ? undefined : hasLocalPassword ? Pencil : KeyRound}
			class="pf-row__action"
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

		{#if passwordDone && !showPasswordForm}
			<p class="pf-row__under flex items-center gap-1.5 text-sm text-success">
				<Check class="size-4" aria-hidden="true" />
				{hasLocalPassword ? 'Password updated' : 'Local password set'}
			</p>
		{/if}

		{#if showPasswordForm}
			<div class="pf-row__under space-y-2">
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
					<p class="text-sm text-muted">
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
					<p class="text-sm text-error">{passwordError}</p>
				{/if}
				<div class="flex gap-2 pt-1">
					<Button
						variant="primary"
						size="display-sm"
						loading={passwordPending}
						disabled={!newPassword || (hasLocalPassword && !currentPassword)}
						onclick={() => void submitPassword()}
					>
						{hasLocalPassword ? 'Update password' : 'Set password'}
					</Button>
					<Button variant="ghost" size="display-sm" onclick={togglePasswordForm}>Cancel</Button>
				</div>
			</div>
		{/if}
	</div>
</div>
