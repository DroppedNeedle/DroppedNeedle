<!--
	The profile's header: the round picture (the button that changes it),
	"Profile" with the role chip, the display name as the page's huge title
	and its edit button, and the ticket and record at the top right.
-->
<script lang="ts">
	import { Camera, Pencil, ShieldCheck, UserCheck } from 'lucide-svelte';
	import { getApiUrl } from '$lib/api/api-utils';
	import { createUpdateDisplayNameMutation } from '$lib/features/profile/queries/profile.svelte';
	import type { ProfileData } from '$lib/features/profile/queries/profile.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import BarcodeTicket from '$lib/ui/BarcodeTicket.svelte';
	import Button from '$lib/ui/Button.svelte';
	import CoverPlaceholder from '$lib/ui/CoverPlaceholder.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import VinylRecord from '$lib/ui/VinylRecord.svelte';
	import AvatarUploadModal from './AvatarUploadModal.svelte';
	import InlineEditField from './InlineEditField.svelte';

	interface Props {
		profile: ProfileData;
	}

	let { profile }: Props = $props();

	const displayNameMutation = createUpdateDisplayNameMutation(authStore.user?.id ?? '');

	let editingName = $state(false);
	let showAvatarModal = $state(false);

	const name = $derived(profile.display_name || 'Set your name');
	// a long name steps down so it wraps less
	const nameScale = $derived(name.length <= 12 ? 1 : name.length <= 20 ? 0.74 : 0.56);
</script>

<header class="pf-head">
	<button
		type="button"
		class="pf-head__avatar"
		onclick={() => (showAvatarModal = true)}
		aria-label="Change profile picture"
	>
		<span class="pf-head__disc">
			{#if profile.avatar_url}
				<img src={getApiUrl(profile.avatar_url)} alt="" class="size-full object-cover" />
			{:else}
				<CoverPlaceholder
					id={profile.id}
					name={profile.username ?? profile.display_name ?? ''}
					kind="artist"
				/>
			{/if}
		</span>
		<span class="pf-head__camera" aria-hidden="true"><Camera class="size-4" /></span>
	</button>

	<div class="pf-head__titles">
		<div class="pf-head__eyebrow">
			<span>Profile</span>
			{#if authStore.user?.role === 'admin'}
				<StatusChip tone="action" icon={ShieldCheck}>Admin</StatusChip>
			{:else if authStore.user?.role === 'trusted'}
				<StatusChip tone="info" icon={UserCheck}>Trusted</StatusChip>
			{:else if authStore.user}
				<StatusChip>User</StatusChip>
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
				class="pf-head__edit"
			/>
		{:else}
			<div class="pf-head__name-row">
				<h1 class="pf-head__name" style:--pf-name-scale={nameScale}>{name}</h1>
				<Button
					variant="outline"
					size="display-sm"
					icon={Pencil}
					onclick={() => (editingName = true)}
				>
					Edit display name
				</Button>
			</div>
		{/if}
	</div>

	<div class="pf-head__deck" aria-hidden="true">
		<VinylRecord sheen="acid" class="pf-head__record" />
		<BarcodeTicket label="DN / CC 3.2" class="pf-head__ticket" />
	</div>
</header>

<AvatarUploadModal bind:open={showAvatarModal} />

<style>
	.pf-head {
		position: relative;
		isolation: isolate;
		overflow: clip;
		display: flex;
		align-items: center;
		gap: 1.75rem;
		padding: 1.5rem 2rem 1.75rem;
	}

	.pf-head__avatar {
		position: relative;
		flex: none;
		width: 9.5rem;
		height: 9.5rem;
		border-radius: 9999px;
		cursor: pointer;
	}
	.pf-head__disc {
		position: absolute;
		inset: 0;
		overflow: hidden;
		border: 4px solid var(--dn-paper);
		border-radius: 9999px;
		box-shadow: 0 10px 30px rgb(0 0 0 / 0.35);
		transition: border-color 150ms;
	}
	.pf-head__avatar:hover .pf-head__disc {
		border-color: var(--dn-title);
	}
	.pf-head__avatar:focus-visible {
		outline: 3px solid var(--dn-focus);
		outline-offset: 3px;
	}
	.pf-head__camera {
		position: absolute;
		right: 0.25rem;
		bottom: 0.375rem;
		display: grid;
		place-items: center;
		width: 2.25rem;
		height: 2.25rem;
		border: 2px solid var(--dn-action-edge);
		border-radius: 9999px;
		background-color: var(--dn-action);
		color: var(--dn-action-content);
		box-shadow: 0 0 0 3px var(--color-base-100);
	}

	.pf-head__titles {
		min-width: 0;
		flex: 0 1 auto;
	}
	/* positioned, so the title's forced-colours backplate never covers it */
	.pf-head__eyebrow {
		position: relative;
		z-index: 1;
		display: flex;
		align-items: center;
		gap: 0.875rem;
		margin-bottom: 0.5rem;
		font-family: var(--font-mono);
		font-size: 1rem;
		font-weight: 600;
		letter-spacing: 0.3em;
		text-transform: uppercase;
	}
	.pf-head__name-row {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.75rem 1.75rem;
	}
	.pf-head__name {
		min-width: 0;
		font-family: var(--font-display);
		font-weight: 400;
		font-size: calc(6.5rem * var(--pf-name-scale, 1));
		line-height: 0.95;
		letter-spacing: 0.005em;
		text-transform: uppercase;
		color: var(--dn-title);
		font-synthesis: none;
		overflow-wrap: anywhere;
	}
	.pf-head__titles :global(.pf-head__edit) {
		margin-top: 0.5rem;
	}

	/* the record and ticket take the room to the right, behind nothing */
	.pf-head__deck {
		position: relative;
		z-index: -1;
		flex: 1 1 0;
		align-self: stretch;
		min-width: 9rem;
		container-type: inline-size;
		pointer-events: none;
	}
	.pf-head__deck :global(.pf-head__record) {
		--size: min(130cqw, 30rem);
		--sheen-from: 150deg;
		position: absolute;
		top: calc(-1.5rem - 0.62 * var(--size));
		right: -6rem;
		width: var(--size);
	}
	.pf-head__deck :global(.pf-head__ticket) {
		position: absolute;
		top: 0.75rem;
		right: 0;
		width: 11.5rem;
	}

	@media (max-width: 1279px) {
		.pf-head__name {
			font-size: calc(5rem * var(--pf-name-scale, 1));
		}
	}

	@media (max-width: 1023px) {
		.pf-head {
			padding-inline: 1.5rem;
		}
		.pf-head__avatar {
			width: 7.5rem;
			height: 7.5rem;
		}
	}

	/* phone: the picture and the ticket in one row, the name under them */
	@media (max-width: 767px) {
		.pf-head {
			display: grid;
			grid-template-columns: auto minmax(0, 1fr);
			grid-template-areas:
				'avatar deck'
				'titles titles';
			gap: 1rem 1rem;
			padding: 1rem 1rem 0.5rem;
		}
		.pf-head__avatar {
			grid-area: avatar;
			width: 6.5rem;
			height: 6.5rem;
		}
		.pf-head__camera {
			right: 0;
			bottom: 0.125rem;
			width: 2rem;
			height: 2rem;
		}
		.pf-head__titles {
			grid-area: titles;
		}
		.pf-head__deck {
			grid-area: deck;
			min-width: 0;
		}
		.pf-head__deck :global(.pf-head__record) {
			--size: 13rem;
			top: -8.5rem;
			right: -4.5rem;
		}
		.pf-head__deck :global(.pf-head__ticket) {
			top: 1.25rem;
			right: 0.25rem;
			width: 7.5rem;
		}
		.pf-head__eyebrow {
			gap: 0.625rem;
			margin-bottom: 0.375rem;
			font-size: 0.75rem;
			letter-spacing: 0.22em;
		}
		.pf-head__name {
			font-size: calc(clamp(2.75rem, 15vw, 3.75rem) * var(--pf-name-scale, 1));
		}
		.pf-head__name-row {
			gap: 0.75rem;
		}
	}

	@media (max-width: 379px) {
		.pf-head__deck :global(.pf-head__ticket) {
			display: none;
		}
	}
</style>
