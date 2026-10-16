<script lang="ts">
	import { Camera, Pencil, ShieldCheck, UserCheck, UserRound } from 'lucide-svelte';
	import { getApiUrl } from '$lib/api/api-utils';
	import { createUpdateDisplayNameMutation } from '$lib/queries/profile/ProfileMutations.svelte';
	import type { ProfileData } from '$lib/queries/profile/types';
	import { authStore } from '$lib/stores/authStore.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import AvatarUploadModal from './AvatarUploadModal.svelte';
	import InlineEditField from './InlineEditField.svelte';

	interface Props {
		profile: ProfileData;
	}

	let { profile }: Props = $props();

	const displayNameMutation = createUpdateDisplayNameMutation(authStore.user?.id ?? '');

	let editingName = $state(false);
	let showAvatarModal = $state(false);

	const roleLabel: Record<string, string> = {
		admin: 'Admin',
		trusted: 'Trusted',
		user: 'User'
	};
</script>

<div class="flex flex-col items-center gap-6">
	<button
		type="button"
		onclick={() => (showAvatarModal = true)}
		class="group relative h-32 w-32 shrink-0 cursor-pointer overflow-hidden rounded-full shadow-2xl ring-4 ring-base-content/10 transition-all hover:ring-primary/40 sm:h-40 sm:w-40"
		aria-label="Change profile picture"
	>
		{#if profile.avatar_url}
			<img
				src={getApiUrl(profile.avatar_url)}
				alt="Profile"
				class="h-full w-full object-cover transition-transform duration-300 group-hover:scale-110"
			/>
		{:else}
			<div
				class="flex h-full w-full items-center justify-center bg-linear-to-br from-primary/30 to-accent/20"
			>
				<UserRound class="h-16 w-16 text-base-content/40 sm:h-20 sm:w-20" />
			</div>
		{/if}
		<div
			class="absolute inset-0 flex items-center justify-center bg-black/50 opacity-0 transition-opacity group-hover:opacity-100"
		>
			<Camera class="h-8 w-8 text-white" />
		</div>
	</button>

	<div class="flex w-full max-w-md flex-col items-center gap-2 pb-2">
		<div class="flex items-center gap-2">
			<span class="text-xs font-semibold uppercase tracking-widest text-base-content/40"
				>Profile</span
			>
			{#if authStore.user}
				{#if authStore.user.role === 'admin'}
					<span class="badge badge-accent badge-sm gap-1">
						<ShieldCheck class="h-3 w-3" />{roleLabel[authStore.user.role]}
					</span>
				{:else if authStore.user.role === 'trusted'}
					<span class="badge badge-info badge-sm gap-1">
						<UserCheck class="h-3 w-3" />{roleLabel[authStore.user.role]}
					</span>
				{:else}
					<span class="badge badge-ghost badge-sm">{roleLabel[authStore.user.role]}</span>
				{/if}
			{/if}
		</div>
		{#if editingName}
			<InlineEditField
				label="Display name"
				value={profile.display_name ?? ''}
				placeholder="Your name"
				pending={displayNameMutation.isPending}
				onsave={(display_name) => displayNameMutation.mutateAsync({ display_name })}
				ondone={() => (editingName = false)}
			/>
		{:else}
			<div class="flex items-center gap-2">
				<h1 class="text-3xl font-bold sm:text-4xl">
					{profile.display_name || 'Set your name'}
				</h1>
				<IconButton
					label="Edit display name"
					icon={Pencil}
					size="sm"
					onclick={() => (editingName = true)}
				/>
			</div>
		{/if}
	</div>
</div>

<AvatarUploadModal bind:open={showAvatarModal} />
