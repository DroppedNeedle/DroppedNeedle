<script lang="ts">
	import { CircleCheck, HardDrive, Layers, Sparkles } from 'lucide-svelte';
	import type { AlbumIdentityState, ArtistIdentityState, NativePickBasis } from '$lib/types';

	interface Props {
		state: AlbumIdentityState | ArtistIdentityState;
		subject: 'album' | 'artist';
		compact?: boolean;
		showDescription?: boolean;
		className?: string;
		pickBasis?: NativePickBasis | null;
	}

	let {
		state,
		subject,
		compact = false,
		showDescription = false,
		className = '',
		pickBasis = null
	}: Props = $props();

	const pickBasisNote: Record<NativePickBasis, string> = {
		pin: 'This pressing was pinned and is shown as the best fit.',
		owned: 'This pressing matches your identified edition and is shown.',
		embedded_tags: 'Your files agree on this pressing, shown as the best fit until verified.'
	};

	const content = $derived.by(() => {
		if (state === 'release_group_linked') {
			if (subject === 'album' && pickBasis && pickBasisNote[pickBasis]) {
				return {
					label: 'Best-fit edition',
					description: `This album is linked to a MusicBrainz release group. ${pickBasisNote[pickBasis]}`
				};
			}
			return {
				label: 'Local edition',
				description:
					'This album is linked to a MusicBrainz release group, but this exact edition is not.'
			};
		}
		if (state === 'custom_edition') {
			return {
				label: 'Custom edition',
				description:
					'File organization uses this administrator-sealed local track list as the edition.'
			};
		}
		if (state === 'release_linked' || state === 'musicbrainz_linked') {
			return {
				label: 'MusicBrainz linked',
				description:
					subject === 'album'
						? 'This exact edition is linked to MusicBrainz.'
						: 'This artist is linked to MusicBrainz.'
			};
		}
		return {
			label: 'Local-only',
			description:
				subject === 'album'
					? 'This album is in your DroppedNeedle library, but no MusicBrainz release is linked yet.'
					: 'This artist is in your DroppedNeedle library, but no MusicBrainz artist is linked yet.'
		};
	});
</script>

<div class="{showDescription ? 'space-y-1.5' : 'inline-flex'} {className}">
	<span
		class="badge badge-outline gap-1.5 border-base-content/20 bg-base-100/90 text-base-content/75 shadow-sm {compact
			? 'badge-sm text-[0.65rem]'
			: ''}"
		aria-label={`${content.label}. ${content.description}`}
		title={content.description}
	>
		{#if state === 'local_only'}
			<HardDrive class={compact ? 'h-3 w-3' : 'h-3.5 w-3.5'} />
		{:else if state === 'release_group_linked'}
			<Layers class={compact ? 'h-3 w-3' : 'h-3.5 w-3.5'} />
		{:else if state === 'custom_edition'}
			<Sparkles class={compact ? 'h-3 w-3' : 'h-3.5 w-3.5'} />
		{:else}
			<CircleCheck class={compact ? 'h-3 w-3' : 'h-3.5 w-3.5'} />
		{/if}
		{content.label}
	</span>
	{#if showDescription}
		<p class="max-w-xl text-sm leading-relaxed text-base-content/55">{content.description}</p>
	{/if}
</div>
