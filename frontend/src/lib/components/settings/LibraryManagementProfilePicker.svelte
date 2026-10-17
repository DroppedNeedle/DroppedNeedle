<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { Check, ChevronDown, CornerDownRight, Layers } from 'lucide-svelte';

	import type { LibraryManagementProfile } from '$lib/queries/library-management/types';

	import { profileAspects } from './managementSettingsDraft.svelte';

	interface Props {
		id: string;
		eyebrow: string;
		label: string;
		description: string;
		profiles: LibraryManagementProfile[];
		selectedId: string;
		changed?: boolean;
		inheritedProfile?: LibraryManagementProfile | null;
		onselect: (profileId: string) => void;
	}

	let {
		id,
		eyebrow,
		label,
		description,
		profiles,
		selectedId,
		changed = false,
		inheritedProfile = null,
		onselect
	}: Props = $props();
	let summary: HTMLButtonElement;
	let expanded = $state(false);

	const inheritsDefault = $derived(selectedId === '' && inheritedProfile !== null);
	const selectedProfile = $derived(
		(selectedId ? profiles.find((profile) => profile.id === selectedId) : inheritedProfile) ??
			profiles[0] ??
			null
	);

	function choose(profileId: string): void {
		onselect(profileId);
		expanded = false;
		summary.focus();
	}
</script>

<div class="management-profile-picker" data-open={expanded}>
	<button
		type="button"
		class="management-profile-picker-summary"
		bind:this={summary}
		aria-label={`Choose ${label}`}
		aria-expanded={expanded}
		aria-controls={`management-profile-options-${id}`}
		onclick={() => (expanded = !expanded)}
	>
		<span class="management-profile-picker-mark" aria-hidden="true"><Layers class="h-5 w-5" /></span
		>
		<span class="min-w-0">
			<span class="management-step">{eyebrow}</span>
			<strong class="management-profile-picker-name">
				{selectedProfile?.name ?? 'Profile unavailable'}
			</strong>
			<span class="management-profile-picker-description">{description}</span>
			{#if selectedProfile}
				<span class="management-profile-picker-aspects" aria-label="Managed capabilities">
					{#each profileAspects(selectedProfile) as aspect (aspect)}
						<span class="management-aspect">{aspect}</span>
					{/each}
				</span>
			{/if}
		</span>
		<span class="management-profile-picker-status">
			{#if changed}<span class="management-unsaved-badge">Unsaved choice</span>{/if}
			<span class="text-xs font-semibold text-base-content/55">
				{inheritsDefault ? 'Inherited' : 'Selected'}
			</span>
			<ChevronDown class="management-profile-picker-chevron h-4 w-4" aria-hidden="true" />
		</span>
	</button>

	{#if expanded}<div class="management-profile-picker-body" id={`management-profile-options-${id}`}>
			<div>
				<h5 class="text-sm font-semibold">{label}</h5>
				<p class="mt-1 text-xs text-base-content/55">{description}</p>
			</div>
			<fieldset class="management-profile-options" aria-label={`${label} choices`}>
				{#if inheritedProfile}
					<label class="management-profile-option">
						<input
							class="sr-only"
							type="radio"
							name={`management-profile-${id}`}
							value=""
							aria-label={`Use library default: ${inheritedProfile.name}`}
							checked={selectedId === ''}
							onchange={() => choose('')}
						/>
						<span class="management-profile-option-icon" aria-hidden="true">
							<CornerDownRight class="h-4 w-4" />
						</span>
						<span class="min-w-0">
							<strong>Use library default</strong>
							<small>{inheritedProfile.name}</small>
							<span class="management-profile-picker-aspects">
								{#each profileAspects(inheritedProfile) as aspect (aspect)}
									<span class="management-aspect">{aspect}</span>
								{/each}
							</span>
						</span>
						<Check class="management-profile-option-check h-4 w-4" aria-hidden="true" />
					</label>
				{/if}

				{#each profiles as profile (profile.id)}
					<label class="management-profile-option">
						<input
							class="sr-only"
							type="radio"
							name={`management-profile-${id}`}
							value={profile.id}
							aria-label={`Use ${profile.name}`}
							checked={selectedId === profile.id}
							onchange={() => choose(profile.id)}
						/>
						<span class="management-profile-option-icon" aria-hidden="true">
							<Layers class="h-4 w-4" />
						</span>
						<span class="min-w-0">
							<span class="flex flex-wrap items-center gap-2">
								<strong>{profile.name}</strong>
								<span
									class="badge badge-xs {profile.preset_origin ? 'badge-outline' : 'badge-ghost'}"
								>
									{profile.preset_origin ? 'Preset' : 'Custom'}
								</span>
							</span>
							<small>{profile.description || 'No description.'}</small>
							<span class="management-profile-picker-aspects">
								{#each profileAspects(profile) as aspect (aspect)}
									<span class="management-aspect">{aspect}</span>
								{/each}
							</span>
						</span>
						<Check class="management-profile-option-check h-4 w-4" aria-hidden="true" />
					</label>
				{/each}
			</fieldset>
		</div>{/if}
</div>

<style>
	.management-profile-picker {
		overflow: hidden;
		border: 1px solid color-mix(in oklab, var(--color-library-manage) 24%, transparent);
		border-radius: 0.95rem;
		background: color-mix(in oklab, var(--color-library-manage) 5%, var(--color-base-100));
	}

	.management-profile-picker-summary {
		display: grid;
		width: 100%;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: start;
		gap: 0.9rem;
		padding: 1rem;
		border: 0;
		background: transparent;
		color: inherit;
		font: inherit;
		text-align: left;
		cursor: pointer;
	}

	.management-profile-picker-summary:focus-visible {
		outline: 2px solid var(--color-library-manage);
		outline-offset: -2px;
	}

	.management-profile-picker[data-open='true'] .management-profile-picker-summary {
		border-bottom: 1px solid color-mix(in oklab, var(--color-library-manage) 18%, transparent);
		background: color-mix(in oklab, var(--color-library-manage) 7%, var(--color-base-100));
	}

	.management-profile-picker-mark,
	.management-profile-option-icon {
		display: grid;
		place-items: center;
		flex: none;
		border: 1px solid color-mix(in oklab, var(--color-library-manage) 22%, transparent);
		background: color-mix(in oklab, var(--color-library-manage) 11%, var(--color-base-100));
		color: color-mix(in oklab, var(--color-library-manage) 68%, var(--color-base-content));
	}

	.management-profile-picker-mark {
		width: 2.4rem;
		height: 2.4rem;
		border-radius: 0.75rem;
	}

	.management-profile-picker-name,
	.management-profile-picker-description {
		display: block;
	}

	.management-profile-picker-name {
		margin-top: 0.1rem;
		font-size: 1rem;
	}

	.management-profile-picker-description {
		margin-top: 0.2rem;
		max-width: 52rem;
		color: color-mix(in oklab, var(--color-base-content) 54%, transparent);
		font-size: 0.75rem;
		line-height: 1.4;
	}

	.management-profile-picker-aspects {
		display: flex;
		flex-wrap: wrap;
		gap: 0.3rem;
		margin-top: 0.55rem;
	}

	.management-profile-picker-status {
		display: flex;
		align-items: center;
		justify-content: end;
		gap: 0.55rem;
		min-height: 2.4rem;
	}

	.management-unsaved-badge {
		border: 1px solid color-mix(in oklab, var(--color-warning) 35%, transparent);
		border-radius: 9999px;
		background: color-mix(in oklab, var(--color-warning) 11%, var(--color-base-100));
		color: color-mix(in oklab, var(--color-warning) 58%, var(--color-base-content));
		padding: 0.2rem 0.55rem;
		font-size: 0.65rem;
		font-weight: 700;
	}

	:global(.management-profile-picker-chevron) {
		transition: transform 160ms ease;
	}

	.management-profile-picker[data-open='true'] :global(.management-profile-picker-chevron) {
		transform: rotate(180deg);
	}

	.management-profile-picker-body {
		display: grid;
		gap: 0.85rem;
		padding: 1rem;
		background: color-mix(in oklab, var(--color-base-200) 45%, var(--color-base-100));
	}

	.management-profile-options {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(min(100%, 19rem), 1fr));
		gap: 0.6rem;
	}

	.management-profile-option {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		align-items: start;
		gap: 0.7rem;
		min-height: 5.5rem;
		padding: 0.8rem;
		border: 1px solid color-mix(in oklab, var(--color-base-content) 12%, transparent);
		border-radius: 0.8rem;
		background: var(--color-base-100);
		cursor: pointer;
		transition:
			border-color 140ms ease,
			background-color 140ms ease,
			transform 140ms ease;
	}

	.management-profile-option:hover {
		border-color: color-mix(in oklab, var(--color-library-manage) 34%, transparent);
		transform: translateY(-1px);
	}

	.management-profile-option:has(input:focus-visible) {
		outline: 2px solid var(--color-library-manage);
		outline-offset: 2px;
	}

	.management-profile-option:has(input:checked) {
		border-color: color-mix(in oklab, var(--color-library-manage) 48%, transparent);
		background: color-mix(in oklab, var(--color-library-manage) 9%, var(--color-base-100));
	}

	.management-profile-option-icon {
		width: 1.8rem;
		height: 1.8rem;
		border-radius: 0.55rem;
	}

	.management-profile-option strong,
	.management-profile-option small {
		display: block;
	}

	.management-profile-option small {
		margin-top: 0.25rem;
		color: color-mix(in oklab, var(--color-base-content) 52%, transparent);
		font-size: 0.7rem;
		line-height: 1.35;
	}

	:global(.management-profile-option-check) {
		margin-top: 0.35rem;
		color: var(--color-library-manage);
		opacity: 0;
	}

	.management-profile-option:has(input:checked) :global(.management-profile-option-check) {
		opacity: 1;
	}

	@media (max-width: 767px) {
		.management-profile-picker-summary {
			grid-template-columns: auto minmax(0, 1fr);
		}

		.management-profile-picker-status {
			grid-column: 1 / -1;
			justify-content: flex-start;
			min-height: auto;
			padding-left: 3.3rem;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.management-profile-option:hover {
			transform: none;
		}
	}
</style>
