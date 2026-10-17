<!--
	The profiles section of the Automation tab: one card per saved profile with
	the library default choice, share, edit and delete, then the row that
	copies a profile or opens the import. Deleting asks first.
-->
<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { Copy, FileUp, Link2, Pencil, Share2, Tags, Trash2 } from 'lucide-svelte';

	import {
		copyLibraryManagementProfileMutation,
		deleteLibraryManagementProfileMutation
	} from '$lib/queries/library-management/LibraryManagementMutations.svelte';
	import type { LibraryRootSettings } from '$lib/queries/library/LibraryOperationsTypes';
	import type { LibraryManagementProfile } from '$lib/queries/library-management/types';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import Input from '$lib/ui/Input.svelte';

	import { profileAspects, type ManagementSettingsDraft } from './managementSettingsDraft.svelte';

	interface Props {
		settings: ManagementSettingsDraft;
		roots: LibraryRootSettings[];
		onedit: (profileId: string) => void;
		onshare: (profile: LibraryManagementProfile) => void;
		onimport: () => void;
		/** Where focus goes when the delete dialog closes and its opener is gone. */
		focusFallback: () => void;
	}

	let { settings, roots, onedit, onshare, onimport, focusFallback }: Props = $props();

	const copyProfile = copyLibraryManagementProfileMutation();
	const deleteProfile = deleteLibraryManagementProfileMutation();

	let newProfileName = $state('Picard-style Organizer copy');
	let deleteDialog: HTMLDialogElement;
	let deleteHeading: HTMLHeadingElement;
	let deleteOpener: HTMLElement | null = null;
	let deleteTarget = $state<LibraryManagementProfile | null>(null);
	let deleteError = $state('');

	async function createProfileCopy(): Promise<void> {
		if (!settings.draft || !settings.copySourceProfileId || !newProfileName.trim()) return;
		settings.saveError = '';
		try {
			const result = await copyProfile.mutateAsync({
				profileId: settings.copySourceProfileId,
				request: {
					name: newProfileName.trim(),
					expected_settings_revision: settings.sourceRevision
				}
			});
			settings.acceptCopy(result);
			onedit(result.profile.id);
		} catch (error) {
			settings.saveError = error instanceof Error ? error.message : 'Could not create the profile.';
		}
	}

	function requestDelete(profile: LibraryManagementProfile, opener: HTMLElement): void {
		deleteTarget = profile;
		deleteError = '';
		deleteOpener = opener;
		deleteDialog.showModal();
		deleteHeading.focus();
	}

	async function confirmDelete(): Promise<void> {
		if (!deleteTarget) return;
		const deletedProfileId = deleteTarget.id;
		try {
			const saved = await deleteProfile.mutateAsync({
				profileId: deleteTarget.id,
				request: { expected_settings_revision: settings.sourceRevision }
			});
			settings.acceptDelete(saved, deletedProfileId);
			deleteDialog.close();
			deleteTarget = null;
		} catch (error) {
			deleteError = error instanceof Error ? error.message : 'Could not delete the profile.';
		}
	}

	function restoreDeleteFocus(): void {
		if (deleteOpener?.isConnected) {
			deleteOpener.focus();
			return;
		}
		focusFallback();
	}
</script>

{#if settings.draft}
	{@const draft = settings.draft}
	<section class="space-y-3" aria-labelledby="management-profiles-title">
		<div>
			<p class="management-step">01 · Profiles</p>
			<h3 id="management-profiles-title" class="font-display text-lg font-semibold">
				Choose what "managed" means
			</h3>
			<p class="text-xs text-base-content/55">
				Choose one library default below. Profiles do nothing until you assign one to a root.
			</p>
		</div>

		<!-- svelte-ignore a11y_no_noninteractive_tabindex (bounded scroll region must be keyboard-focusable) -->
		<div
			class="management-profile-grid"
			role="region"
			aria-label="Saved organization profiles"
			tabindex="0"
		>
			{#each settings.profiles as profile (profile.id)}
				{@const rootLabels = settings.rootLabelsFor(profile.id, roots)}
				<article
					class="management-profile-card"
					data-default={profile.id === draft.default_profile_id}
					aria-labelledby={`management-profile-${profile.id}`}
				>
					<div class="flex items-start justify-between gap-3">
						<div class="min-w-0">
							<div class="flex flex-wrap items-center gap-2">
								<h4 id={`management-profile-${profile.id}`} class="font-semibold">
									{profile.name}
								</h4>
								<span
									class="badge badge-xs {profile.preset_origin ? 'badge-outline' : 'badge-ghost'}"
									>{profile.preset_origin ? 'Preset' : 'Custom'}</span
								>
							</div>
							<p class="mt-1 line-clamp-2 text-xs text-base-content/55">
								{profile.description || 'No description.'}
							</p>
						</div>
						<Tags class="h-5 w-5 shrink-0 text-library-manage" />
					</div>
					<div class="mt-3 flex flex-wrap gap-1.5">
						{#each profileAspects(profile) as aspect (aspect)}<span class="management-aspect"
								>{aspect}</span
							>{/each}
					</div>
					<div class="management-profile-card__footer">
						<div class="management-profile-card__state">
							<label
								class="management-profile-default-control"
								data-unsaved={profile.id === draft.default_profile_id &&
									draft.default_profile_id !== settings.persisted?.default_profile_id}
							>
								<input
									type="radio"
									name="library-default-profile"
									class="radio radio-sm"
									checked={profile.id === draft.default_profile_id}
									aria-label={`Make ${profile.name} the library default`}
									onchange={() => settings.setDefaultProfile(profile.id)}
								/>
								<span
									>{profile.id !== draft.default_profile_id
										? 'Make default'
										: draft.default_profile_id !== settings.persisted?.default_profile_id
											? 'Default after save'
											: 'Library default'}</span
								>
							</label>
							<span
								class="text-xs font-medium text-base-content/55"
								title={rootLabels.length ? `Assigned to: ${rootLabels.join(', ')}` : undefined}
								>{#if rootLabels.length}<Link2
										class="mr-1 inline h-3.5 w-3.5 align-text-bottom text-library-manage"
									/>Assigned to {rootLabels.join(', ')}{:else}Not assigned to a root{/if}</span
							>
						</div>
						<div class="management-profile-card__actions">
							<Button
								variant="ghost"
								size="xs"
								icon={Share2}
								aria-label={`Share ${profile.name}`}
								onclick={() => onshare(profile)}>Share</Button
							>
							<Button variant="ghost" size="xs" icon={Pencil} onclick={() => onedit(profile.id)}
								>Edit</Button
							>
							{#if !profile.preset_origin}
								{@const deleteReason = settings.deleteBlocker(profile, roots)}
								{#if deleteReason}
									<Button
										variant="ghost"
										size="xs"
										icon={Trash2}
										aria-label={`Cannot delete ${profile.name}: ${deleteReason}`}
										disabled>{deleteReason}</Button
									>
								{:else}
									<IconButton
										variant="danger-ghost"
										size="xs"
										shape="square"
										icon={Trash2}
										label={`Delete ${profile.name}`}
										onclick={(event: MouseEvent & { currentTarget: HTMLElement }) =>
											requestDelete(profile, event.currentTarget)}
									/>
								{/if}
							{/if}
						</div>
					</div>
				</article>
			{/each}
		</div>

		<div class="management-create-row">
			<div>
				<strong class="text-sm">Copy an existing profile</strong>
				<p class="text-xs text-base-content/50">
					Copies every saved value without changing root assignments or enabling automation.
				</p>
			</div>
			<select
				class="select select-sm min-w-52"
				bind:value={settings.copySourceProfileId}
				aria-label="Profile to copy"
			>
				{#each settings.profiles as profile (profile.id)}<option value={profile.id}
						>{profile.name}{profile.preset_origin ? ' · preset based' : ' · custom'}</option
					>{/each}
			</select>
			<Input size="sm" class="min-w-52" label="New profile name" bind:value={newProfileName} />
			<Button
				variant="outline"
				size="sm"
				icon={Copy}
				disabled={copyProfile.isPending || !settings.copySourceProfileId || !newProfileName.trim()}
				onclick={() => void createProfileCopy()}>Create copy</Button
			>
			<Button
				variant="outline"
				size="sm"
				icon={FileUp}
				disabled={settings.hasUnsaved}
				title={settings.hasUnsaved
					? 'Save or discard current organization changes before importing.'
					: undefined}
				onclick={onimport}>Import profile</Button
			>
		</div>
		{#if settings.hasUnsaved}<p class="text-xs text-warning">
				Save or discard current organization changes before importing a profile.
			</p>{/if}
	</section>
{/if}

<dialog
	bind:this={deleteDialog}
	class="modal"
	aria-labelledby="delete-management-profile-title"
	onclose={restoreDeleteFocus}
	oncancel={(event) => {
		if (deleteProfile.isPending) event.preventDefault();
	}}
>
	<div class="modal-box max-w-md">
		<h2
			bind:this={deleteHeading}
			id="delete-management-profile-title"
			tabindex="-1"
			class="text-lg font-bold"
		>
			Delete {deleteTarget?.name ?? 'profile'}?
		</h2>
		<p class="mt-3 text-sm text-base-content/65">
			This removes only the profile. Assigned profiles cannot be deleted, and no music file is
			touched.
		</p>
		{#if deleteError}<div class="alert alert-error mt-3 text-sm" role="alert">
				{deleteError}
			</div>{/if}
		<div class="modal-action">
			<Button
				variant="ghost"
				disabled={deleteProfile.isPending}
				onclick={() => deleteDialog.close()}>Cancel</Button
			><Button
				variant="danger"
				loading={deleteProfile.isPending}
				onclick={() => void confirmDelete()}>Delete profile</Button
			>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Cancel profile deletion" disabled={deleteProfile.isPending}>close</button>
	</form>
</dialog>

<style>
	.management-profile-grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 17rem), 1fr));
		gap: 0.75rem;
		max-height: min(28rem, 65vh);
		overflow-y: auto;
		overscroll-behavior: contain;
		padding: 0.15rem 0.35rem 0.75rem 0.15rem;
		scroll-padding-block-end: 0.75rem;
		scrollbar-gutter: stable;
	}

	.management-profile-grid:focus-visible {
		border-radius: 0.25rem;
		outline: 2px solid var(--color-library-manage);
		outline-offset: 0.25rem;
	}

	.management-profile-card {
		border: 1px solid color-mix(in oklab, var(--color-base-content) 11%, transparent);
		border-radius: 0.9rem;
		background: color-mix(in oklab, var(--color-base-100) 88%, var(--color-base-200));
	}

	.management-profile-card {
		display: flex;
		min-width: 0;
		flex-direction: column;
		padding: 1rem;
		transition:
			border-color 160ms ease,
			box-shadow 160ms ease,
			background-color 160ms ease;
	}

	.management-profile-card__footer {
		display: flex;
		align-items: stretch;
		flex-direction: column;
		gap: 0.75rem;
		margin-top: auto;
		padding-top: 0.75rem;
		border-top: 1px solid color-mix(in oklab, var(--color-base-content) 10%, transparent);
	}

	.management-profile-card__actions {
		display: flex;
		flex-wrap: wrap;
		justify-content: flex-end;
		gap: 0.25rem;
	}

	.management-profile-card[data-default='true'] {
		border-color: color-mix(in oklab, var(--color-library-manage) 50%, transparent);
		background: color-mix(in oklab, var(--color-library-manage) 7%, var(--color-base-100));
		box-shadow:
			0 0 0 1px color-mix(in oklab, var(--color-library-manage) 20%, transparent),
			0 0 1.4rem color-mix(in oklab, var(--color-library-manage) 12%, transparent);
	}

	.management-profile-card__state {
		display: flex;
		min-width: 0;
		flex-direction: column;
		align-items: flex-start;
		gap: 0.35rem;
	}

	.management-profile-default-control {
		display: inline-flex;
		align-items: center;
		gap: 0.45rem;
		min-height: 1.75rem;
		padding-inline: 0.6rem;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 13%, transparent);
		border-radius: 9999px;
		background: color-mix(in oklab, var(--color-base-200) 65%, var(--color-base-100));
		color: color-mix(in oklab, var(--color-base-content) 58%, transparent);
		font-size: 0.7rem;
		font-weight: 700;
		white-space: nowrap;
		cursor: pointer;
	}

	.management-profile-default-control:has(input:checked) {
		border-color: color-mix(in oklab, var(--color-library-manage) 38%, transparent);
		background: color-mix(in oklab, var(--color-library-manage) 12%, var(--color-base-100));
		color: color-mix(in oklab, var(--color-library-manage) 64%, var(--color-base-content));
	}

	.management-profile-default-control[data-unsaved='true'] {
		border-color: color-mix(in oklab, var(--color-warning) 38%, transparent);
		background: color-mix(in oklab, var(--color-warning) 10%, var(--color-base-100));
		color: color-mix(in oklab, var(--color-warning) 58%, var(--color-base-content));
	}

	.management-create-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
		padding: 0.85rem 1rem;
		border: 1px dashed color-mix(in oklab, var(--color-base-content) 16%, transparent);
		border-radius: 0.9rem;
		background: color-mix(in oklab, var(--color-base-200) 58%, transparent);
	}

	@media (max-width: 639px) {
		.management-create-row {
			align-items: stretch;
			flex-direction: column;
		}

		.management-profile-card__actions {
			justify-content: flex-start;
		}
	}
</style>
