<!--
	The profile's name and description, and for a preset-based profile how it
	differs from its preset, with a reset button per changed group.
-->
<script lang="ts" module>
	export type PresetGroup =
		| 'metadata'
		| 'genres'
		| 'artwork'
		| 'organization'
		| 'file_behavior'
		| 'enrichment'
		| 'notification';

	export const presetGroupLabels: Record<PresetGroup, string> = {
		metadata: 'Metadata',
		genres: 'Genres',
		artwork: 'Artwork',
		organization: 'File organization',
		file_behavior: 'File safety',
		enrichment: 'Lyrics and ReplayGain',
		notification: 'Notifications'
	};
</script>

<script lang="ts">
	import '$lib/components/library/library-management.css';
	import { RotateCcw } from 'lucide-svelte';

	import type { LibraryManagementProfile } from '$lib/queries/library-management/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import TextField from '$lib/ui/TextField.svelte';

	interface Props {
		draft: LibraryManagementProfile;
		/** Groups that differ from the preset; null until the comparison loads. */
		changedGroups: PresetGroup[] | null;
		compareFailed: boolean;
		onreset: (group: PresetGroup, opener: HTMLElement) => void;
	}

	let { draft = $bindable(), changedGroups, compareFailed, onreset }: Props = $props();
</script>

<section class="management-editor-section grid gap-4 lg:grid-cols-2">
	<TextField label="Name" bind:value={draft.name} />
	<TextField label="Description" bind:value={draft.description} />
	<div class="lg:col-span-2 flex flex-wrap gap-2 text-xs">
		<span class="badge badge-outline">{draft.preset_origin ? 'Preset based' : 'Custom'}</span>
		<span class="badge badge-ghost font-mono">rev {draft.revision.slice(0, 8)}</span>
	</div>
	{#if draft.preset_origin && changedGroups}
		<Card variant="bordered" padding="sm" class="lg:col-span-2 text-sm">
			<strong>{changedGroups.length > 0 ? 'Customized from preset' : 'Matches saved preset'}</strong
			>
			<p class="text-xs text-base-content/55">
				{changedGroups.length > 0
					? `Changed sections: ${changedGroups.map((group) => presetGroupLabels[group].toLowerCase()).join(', ')}`
					: 'No profile sections differ from its recorded preset version.'}
			</p>
			{#if changedGroups.length > 0}
				<div class="mt-1 flex flex-wrap gap-2">
					{#each changedGroups as group (group)}
						<Button
							variant="ghost"
							size="xs"
							icon={RotateCcw}
							onclick={(event: MouseEvent & { currentTarget: HTMLElement }) =>
								onreset(group, event.currentTarget)}
						>
							Reset {presetGroupLabels[group]}
						</Button>
					{/each}
				</div>
			{/if}
		</Card>
	{:else if draft.preset_origin && compareFailed}
		<div class="alert alert-error lg:col-span-2 py-2 text-xs" role="alert">
			Could not compare this profile with its preset. Saved values are unchanged.
		</div>
	{/if}
</section>
