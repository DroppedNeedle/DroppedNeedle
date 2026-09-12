<script lang="ts">
	import { Check, ChevronDown, CornerDownRight, Layers } from 'lucide-svelte';

	import type { LibraryManagementProfile } from '$lib/queries/library-management/types';

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

	function profileAspects(profile: LibraryManagementProfile): string[] {
		const aspects: string[] = [];
		if (profile.metadata.enabled) aspects.push('tags');
		if (profile.genres.enabled) aspects.push('genres');
		if (profile.artwork.embedded_enabled || profile.artwork.external_enabled)
			aspects.push('artwork');
		if (profile.enrichment.lyrics.enabled) aspects.push('lyrics');
		if (profile.enrichment.replaygain.enabled) aspects.push('ReplayGain');
		if (profile.organization.rename_enabled) aspects.push('rename');
		if (profile.organization.move_enabled) aspects.push('move');
		return aspects;
	}

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
		<span class="management-profile-picker-mark" aria-hidden="true"
			><Layers class="h-5 w-5" /></span
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
